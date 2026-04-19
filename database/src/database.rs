use chrono::DateTime;
use rustls::ClientConfig;
use rustls_native_certs::load_native_certs;
use serde::{Deserialize, Serialize};
use std::{env, error::Error};
use tokio_postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{error, info};

pub struct DatabaseClient {
    pub db: Client,
}

#[derive(Default, Clone, Debug)]
pub struct Email {
    pub sender: String,
    pub recipients: Vec<String>,
    pub content: String,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MailRow {
    pub id: i64,
    pub date: String,
    pub sender: String,
    pub recipients: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddress {
    pub address: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddressInfo {
    pub address: String,
    pub created_at: Option<String>,
    pub email_count: i64,
}

impl DatabaseClient {
    pub async fn connect() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let host = env::var("DB_HOST").expect("DB_HOST not set");
        let user = env::var("DB_USER").expect("DB_USER not set");
        let password = env::var("DB_PASSWORD").expect("DB_PASSWORD not set");
        let dbname = env::var("DB_NAME").expect("DB_NAME not set");
        let port = env::var("DB_PORT").unwrap_or_else(|_| "5432".to_string());
        let sslmode = env::var("DB_SSLMODE").unwrap_or_else(|_| "require".to_string());

        let connection_string = format!(
            "host={} port={} user={} password={} dbname={} sslmode={}",
            host, port, user, password, dbname, sslmode
        );

        let mut root_store = rustls::RootCertStore::empty();
        let certs = load_native_certs();
        for cert in certs.certs {
            let _ = root_store.add(cert);
        }

        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let tls = MakeRustlsConnect::new(tls_config);

        let (client, connection) = match tokio_postgres::connect(&connection_string, tls).await {
            Ok((client, connection)) => (client, connection),
            Err(e) => {
                error!("Failed to connect to the database: {}", e);
                return Err(Box::new(e));
            }
        };

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("Connection error: {}", e);
            }
        });

        let sql: &str = r#"
            CREATE TABLE IF NOT EXISTS mail (
                id BIGSERIAL PRIMARY KEY,
                date TEXT,
                sender TEXT,
                recipients TEXT,
                data TEXT
            );
            CREATE INDEX IF NOT EXISTS mail_date ON mail(date);
            CREATE INDEX IF NOT EXISTS mail_recipients ON mail(recipients);
            CREATE INDEX IF NOT EXISTS mail_date_recipients ON mail(date, recipients);

            CREATE TABLE IF NOT EXISTS quota (
                id SERIAL PRIMARY KEY,
                address TEXT NOT NULL UNIQUE,
                quota_limit INTEGER NOT NULL,
                completed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS quota_address_idx ON quota(address);

            CREATE TABLE IF NOT EXISTS user_config (
                id SERIAL PRIMARY KEY,
                mail TEXT NOT NULL UNIQUE,
                address TEXT NOT NULL,
                web_hook_address TEXT
            );
            CREATE INDEX IF NOT EXISTS user_config_mail_idx ON user_config(mail);

            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1
                    FROM information_schema.table_constraints
                    WHERE constraint_name = 'fk_user_config_address'
                    AND table_name = 'user_config'
                ) THEN
                    ALTER TABLE user_config
                        ADD CONSTRAINT fk_user_config_address
                        FOREIGN KEY (address)
                        REFERENCES quota(address)
                        ON DELETE CASCADE
                        ON UPDATE CASCADE;
                END IF;
            END;
            $$;

            CREATE TABLE IF NOT EXISTS email_addresses (
                id BIGSERIAL PRIMARY KEY,
                address TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (now()::text)
            );
            CREATE INDEX IF NOT EXISTS email_addresses_address_idx ON email_addresses(address);
        "#;

        if let Err(e) = client.batch_execute(sql).await {
            error!("Failed to execute initialization queries: {}", e);
            return Err(Box::new(e));
        }

        info!("Database initialized successfully");
        Ok(DatabaseClient { db: client })
    }

    // ============== MAIL OPERATIONS ==============

    pub async fn add_mail(&self, data: Email) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let sql: &str = "INSERT INTO mail (date, sender, recipients, data) VALUES ($1, $2, $3, $4)";
        let date: String = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string();

        match self
            .db
            .execute(
                sql,
                &[&date, &data.sender, &data.recipients[0], &data.content],
            )
            .await
        {
            Ok(rows_affected) => Ok(rows_affected),
            Err(e) => {
                error!("Failed to add mail to the database: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_old_mail(&self) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let now: DateTime<chrono::Utc> = chrono::offset::Utc::now();
        let a_week_ago: DateTime<chrono::Utc> = now - chrono::Duration::days(7);
        let a_week_ago: String = a_week_ago.format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        info!("Deleting old mail from before {}", a_week_ago);
        match self
            .db
            .execute("DELETE FROM mail WHERE date < $1", &[&a_week_ago])
            .await
        {
            Ok(rows) => Ok(rows),
            Err(e) => {
                error!("Failed to delete old mail: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn get_mails_by_recipient(
        &self,
        recipient: &str,
    ) -> Result<Vec<MailRow>, Box<dyn Error + Send + Sync>> {
        let sql =
            "SELECT id, date, sender, recipients, data FROM mail WHERE recipients = $1 ORDER BY date DESC";
        match self.db.query(sql, &[&recipient]).await {
            Ok(rows) => {
                let mails: Vec<MailRow> = rows
                    .into_iter()
                    .map(|row| MailRow {
                        id: row.get(0),
                        date: row.get(1),
                        sender: row.get(2),
                        recipients: row.get(3),
                        data: row.get(4),
                    })
                    .collect();
                Ok(mails)
            }
            Err(e) => {
                error!("Failed to get mails: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn get_mail_by_id(&self, id: i64) -> Result<Option<MailRow>, Box<dyn Error + Send + Sync>> {
        let sql = "SELECT id, date, sender, recipients, data FROM mail WHERE id = $1";
        match self.db.query_one(sql, &[&id]).await {
            Ok(row) => Ok(Some(MailRow {
                id: row.get(0),
                date: row.get(1),
                sender: row.get(2),
                recipients: row.get(3),
                data: row.get(4),
            })),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get mail by id: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_mail(&self, id: i64) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let sql = "DELETE FROM mail WHERE id = $1";
        match self.db.execute(sql, &[&id]).await {
            Ok(rows) => Ok(rows),
            Err(e) => {
                error!("Failed to delete mail: {}", e);
                Err(Box::new(e))
            }
        }
    }

    // ============== QUOTA OPERATIONS ==============

    pub async fn get_quota(&self, address: &str) -> Result<Option<(i32, i32)>, Box<dyn Error + Send + Sync>> {
        let sql = "SELECT quota_limit, completed FROM quota WHERE address = $1";
        match self.db.query_one(sql, &[&address]).await {
            Ok(row) => Ok(Some((row.get(0), row.get(1)))),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn increment_quota(&self, address: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sql = "UPDATE quota SET completed = completed + 1 WHERE address = $1";
        match self.db.execute(sql, &[&address]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to increment quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn create_quota(&self, address: &str, limit: i32) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sql = "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, $2, 0) ON CONFLICT (address) DO NOTHING";
        match self.db.execute(sql, &[&address, &limit]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to create quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    // ============== WEBHOOK OPERATIONS ==============

    pub async fn get_webhook_url(&self, mail: &str) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
        let sql = "SELECT web_hook_address FROM user_config WHERE mail = $1";
        match self.db.query_one(sql, &[&mail]).await {
            Ok(row) => Ok(row.get(0)),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get webhook url: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn set_webhook(&self, mail: &str, webhook_url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sql = "INSERT INTO user_config (mail, address, web_hook_address) VALUES ($1, $2, $3) ON CONFLICT (mail) DO UPDATE SET web_hook_address = $3";
        let address = mail.split('@').next().unwrap_or(mail);
        match self.db.execute(sql, &[&mail, &address, &webhook_url]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to set webhook: {}", e);
                Err(Box::new(e))
            }
        }
    }

    // ============== EMAIL ADDRESS OPERATIONS (for HTTP API) ==============

    pub async fn create_email_address(&self, username: &str) -> Result<EmailAddress, Box<dyn Error + Send + Sync>> {
        let domain = env::var("MAIL_DOMAIN").unwrap_or_else(|_| "mail.kreyon.in".to_string());
        let address = format!("{}@{}", username, domain);
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        // Create quota entry with default limit
        let default_quota_limit: i32 = 1000;
        if let Err(e) = self.create_quota(&address, default_quota_limit).await {
            error!("Failed to create quota for {}: {}", address, e);
        }

        let sql = "INSERT INTO email_addresses (address, created_at) VALUES ($1, $2) RETURNING address, created_at";
        match self.db.query_one(sql, &[&address, &created_at]).await {
            Ok(row) => Ok(EmailAddress {
                address: row.get(0),
                created_at: row.get(1),
            }),
            Err(e) if e.to_string().contains("duplicate key") => {
                Err("Email address already exists".into())
            }
            Err(e) => {
                error!("Failed to create email address: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_email_address(&self, address: &str) -> Result<bool, Box<dyn Error + Send + Sync>> {
        // Delete from email_addresses
        let sql = "DELETE FROM email_addresses WHERE address = $1";
        match self.db.execute(sql, &[&address]).await {
            Ok(rows) => Ok(rows > 0),
            Err(e) => {
                error!("Failed to delete email address: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn list_email_addresses(&self) -> Result<Vec<EmailAddressInfo>, Box<dyn Error + Send + Sync>> {
        let sql = r#"
            SELECT
                ea.address,
                ea.created_at,
                COALESCE((SELECT COUNT(*) FROM mail m WHERE m.recipients = ea.address), 0) as email_count
            FROM email_addresses ea
            ORDER BY ea.created_at DESC
        "#;
        match self.db.query(sql, &[]).await {
            Ok(rows) => {
                let addresses: Vec<EmailAddressInfo> = rows
                    .into_iter()
                    .map(|row| EmailAddressInfo {
                        address: row.get(0),
                        created_at: row.get(1),
                        email_count: row.get(2),
                    })
                    .collect();
                Ok(addresses)
            }
            Err(e) => {
                error!("Failed to list email addresses: {}", e);
                Err(Box::new(e))
            }
        }
    }
}
