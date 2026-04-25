use chrono::DateTime;
use deadpool_postgres::{Manager, Pool, Runtime};
use rustls::ClientConfig;
use rustls_native_certs::load_native_certs;
use serde::{Deserialize, Serialize};
use std::{env, error::Error};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{error, info};

pub struct DatabaseClient {
    pub pool: Pool,
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
    pub id: String,
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
        let mut pg_config = tokio_postgres::Config::new();
        pg_config.host(&env::var("DB_HOST").expect("DB_HOST not set"));
        pg_config.user(&env::var("DB_USER").expect("DB_USER not set"));
        pg_config.password(&env::var("DB_PASSWORD").expect("DB_PASSWORD not set"));
        pg_config.dbname(&env::var("DB_NAME").expect("DB_NAME not set"));
        pg_config.port(
            env::var("DB_PORT")
                .unwrap_or_else(|_| "5432".to_string())
                .parse()?,
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
        let manager = Manager::new(pg_config, tls);
        let pool = Pool::builder(manager)
            .max_size(16)
            .runtime(Runtime::Tokio1)
            .build()?;

        // Initialize database schema
        let client = pool.get().await?;
        let sql: &str = r#"
            CREATE EXTENSION IF NOT EXISTS "pgcrypto";

            CREATE TABLE IF NOT EXISTS mail (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                date TEXT,
                sender TEXT,
                recipients TEXT,
                data TEXT
            );
            CREATE INDEX IF NOT EXISTS mail_date ON mail(date);
            CREATE INDEX IF NOT EXISTS mail_recipients ON mail(recipients);
            CREATE INDEX IF NOT EXISTS mail_date_recipients ON mail(date, recipients);

            CREATE TABLE IF NOT EXISTS quota (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                address TEXT NOT NULL UNIQUE,
                quota_limit INTEGER NOT NULL,
                completed INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS quota_address_idx ON quota(address);

            CREATE TABLE IF NOT EXISTS user_config (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
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
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                address TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (now()::text)
            );
            CREATE INDEX IF NOT EXISTS email_addresses_address_idx ON email_addresses(address);

            CREATE TABLE IF NOT EXISTS analytics (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                event_type TEXT NOT NULL,
                event_count BIGINT NOT NULL DEFAULT 0,
                last_updated TEXT NOT NULL DEFAULT (now()::text)
            );
            CREATE INDEX IF NOT EXISTS analytics_event_type_idx ON analytics(event_type);
        "#;

        if let Err(e) = client.batch_execute(sql).await {
            error!("Failed to execute initialization queries: {}", e);
            return Err(Box::new(e));
        }

        info!("Database initialized successfully with connection pooling");
        Ok(DatabaseClient { pool })
    }

    // ============== MAIL OPERATIONS ==============

    pub async fn add_mail(&self, data: Email) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let mut retry_count = 0;
        let max_retries = 3;

        while retry_count < max_retries {
            match self.pool.get().await {
                Ok(client) => {
                    let sql: &str = "INSERT INTO mail (date, sender, recipients, data) VALUES ($1, $2, $3, $4)";
                    let date: String = chrono::Utc::now()
                        .format("%Y-%m-%d %H:%M:%S%.3f")
                        .to_string();

                    // Normalize recipient: strip angle brackets if present
                    let normalized_recipient = data.recipients[0]
                        .trim_start_matches('<')
                        .trim_end_matches('>');

                    match client
                        .execute(
                            sql,
                            &[&date, &data.sender, &normalized_recipient, &data.content],
                        )
                        .await
                    {
                        Ok(rows_affected) => return Ok(rows_affected),
                        Err(e) if retry_count < max_retries - 1 => {
                            error!("add_mail failed (attempt {}): {}", retry_count + 1, e);
                            retry_count += 1;
                            tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                            continue;
                        }
                        Err(e) => {
                            error!("Failed to add mail to the database: {}", e);
                            return Err(Box::new(e));
                        }
                    }
                }
                Err(e) if retry_count < max_retries - 1 => {
                    error!("Failed to get connection from pool (attempt {}): {}", retry_count + 1, e);
                    retry_count += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                }
                Err(e) => {
                    error!("Failed to get connection from pool: {}", e);
                    return Err(Box::new(e));
                }
            }
        }
        Err("Max retries exceeded".into())
    }

    pub async fn delete_old_mail(&self) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let now: DateTime<chrono::Utc> = chrono::offset::Utc::now();
        let a_day_ago: DateTime<chrono::Utc> = now - chrono::Duration::days(1);
        let a_day_ago: String = a_day_ago.format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        info!("Deleting old mail from before {}", a_day_ago);
        match client
            .execute("DELETE FROM mail WHERE date < $1", &[&a_day_ago])
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
        let client = self.pool.get().await?;
        let normalized_recipient = recipient.trim_start_matches('<').trim_end_matches('>');
        let sql =
            "SELECT id, date, sender, recipients, data FROM mail WHERE recipients = $1 ORDER BY date DESC";
        match client.query(sql, &[&normalized_recipient]).await {
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

    pub async fn get_mail_by_id(&self, id: &str) -> Result<Option<MailRow>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT id, date, sender, recipients, data FROM mail WHERE id = $1";
        match client.query_one(sql, &[&id]).await {
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

    pub async fn delete_mail(&self, id: &str) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "DELETE FROM mail WHERE id = $1";
        match client.execute(sql, &[&id]).await {
            Ok(rows) => Ok(rows),
            Err(e) => {
                error!("Failed to delete mail: {}", e);
                Err(Box::new(e))
            }
        }
    }

    // ============== QUOTA OPERATIONS ==============

    pub async fn get_quota(&self, address: &str) -> Result<Option<(i32, i32)>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT quota_limit, completed FROM quota WHERE address = $1";
        match client.query_one(sql, &[&address]).await {
            Ok(row) => Ok(Some((row.get(0), row.get(1)))),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn increment_quota(&self, address: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "UPDATE quota SET completed = completed + 1 WHERE address = $1";
        match client.execute(sql, &[&address]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to increment quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn create_quota(&self, address: &str, limit: i32) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut retry_count = 0;
        let max_retries = 3;

        while retry_count < max_retries {
            match self.pool.get().await {
                Ok(client) => {
                    let sql = "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, $2, 0) ON CONFLICT (address) DO NOTHING";
                    match client.execute(sql, &[&address, &limit]).await {
                        Ok(_) => return Ok(()),
                        Err(e) if retry_count < max_retries - 1 => {
                            error!("Failed to create quota (attempt {}): {}", retry_count + 1, e);
                            retry_count += 1;
                            tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                            continue;
                        }
                        Err(e) => {
                            error!("Failed to create quota: {}", e);
                            return Err(Box::new(e));
                        }
                    }
                }
                Err(e) if retry_count < max_retries - 1 => {
                    error!("Failed to get connection for quota (attempt {}): {}", retry_count + 1, e);
                    retry_count += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                }
                Err(e) => {
                    error!("Failed to get connection for quota: {}", e);
                    return Err(Box::new(e));
                }
            }
        }
        Err("Max retries exceeded".into())
    }

    // ============== WEBHOOK OPERATIONS ==============

    pub async fn get_webhook_url(&self, mail: &str) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT web_hook_address FROM user_config WHERE mail = $1";
        match client.query_one(sql, &[&mail]).await {
            Ok(row) => Ok(row.get(0)),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get webhook url: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn set_webhook(&self, mail: &str, webhook_url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "INSERT INTO user_config (mail, address, web_hook_address) VALUES ($1, $2, $3) ON CONFLICT (mail) DO UPDATE SET web_hook_address = $3";
        let address = mail.split('@').next().unwrap_or(mail);
        match client.execute(sql, &[&mail, &address, &webhook_url]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to set webhook: {}", e);
                Err(Box::new(e))
            }
        }
    }

    // ============== EMAIL ADDRESS OPERATIONS (for HTTP API) ==============

    pub async fn create_email_address(&self, username: &str) -> Result<EmailAddress, Box<dyn Error + Send + Sync>> {
        let domain = env::var("MAIL_DOMAIN").unwrap_or_else(|_| "xelio.me".to_string());
        let address = format!("{}@{}", username, domain);
        let created_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        let default_quota_limit: i32 = 1000;
        let mut retry_count = 0;
        let max_retries = 3;

        // Create or restore quota entry with retry
        while retry_count < max_retries {
            match self.pool.get().await {
                Ok(client) => {
                    // Use DO UPDATE to restore deleted quota entries
                    let sql = "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, $2, 0) ON CONFLICT (address) DO UPDATE SET quota_limit = EXCLUDED.quota_limit, completed = 0";
                    match client.execute(sql, &[&address, &default_quota_limit]).await {
                        Ok(_) => break,
                        Err(e) if retry_count < max_retries - 1 => {
                            error!("Failed to create quota for {} (attempt {}): {}", address, retry_count + 1, e);
                            retry_count += 1;
                            tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                        }
                        Err(e) => {
                            error!("Failed to create quota for {}: {}", address, e);
                            let err_str = e.to_string();
                            if err_str.contains("duplicate key") {
                                return Err(format!("Email address '{}' already exists in quota system", address).into());
                            }
                            return Err(format!("Database error creating quota: {}", e).into());
                        }
                    }
                }
                Err(e) if retry_count < max_retries - 1 => {
                    error!("Failed to get connection (attempt {}): {}", retry_count + 1, e);
                    retry_count += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                }
                Err(e) => {
                    error!("Failed to get connection: {}", e);
                    return Err("Database connection failed. Please check if the database server is running.".into());
                }
            }
        }

        retry_count = 0;
        while retry_count < max_retries {
            match self.pool.get().await {
                Ok(client) => {
                    // Use DO NOTHING for true "create only if not exists" semantics
                    let sql = "INSERT INTO email_addresses (address, created_at) VALUES ($1, $2) ON CONFLICT (address) DO NOTHING";
                    match client.execute(sql, &[&address, &created_at]).await {
                        Ok(rows) => {
                            if rows == 0 {
                                // Conflict - address already exists
                                return Err(format!("Username '{}' is already taken. Please choose a different username.", username).into());
                            }
                            return Ok(EmailAddress {
                                address: address.clone(),
                                created_at: Some(created_at),
                            });
                        }
                        Err(e) if retry_count < max_retries - 1 => {
                            error!("Failed to create email address (attempt {}): {}", retry_count + 1, e);
                            retry_count += 1;
                            tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                            continue;
                        }
                        Err(e) => {
                            error!("Failed to create email address: {}", e);
                            let err_str = e.to_string();
                            if err_str.contains("duplicate key") {
                                return Err(format!("Username '{}' is already taken. Please choose a different username.", username).into());
                            }
                            return Err(format!("Database error: {}", e).into());
                        }
                    }
                }
                Err(e) if retry_count < max_retries - 1 => {
                    error!("Failed to get connection (attempt {}): {}", retry_count + 1, e);
                    retry_count += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(100 * (retry_count as u64))).await;
                }
                Err(e) => {
                    error!("Failed to get connection: {}", e);
                    return Err("Database connection failed. Please check if the database server is running.".into());
                }
            }
        }
        Err("Failed to create email address after multiple retries. Please try again.".into())
    }

    pub async fn delete_email_address(&self, address: &str) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        // Delete from quota first (due to foreign key constraint)
        let sql_quota = "DELETE FROM quota WHERE address = $1";
        if let Err(e) = client.execute(sql_quota, &[&address]).await {
            error!("Failed to delete quota for {}: {}", address, e);
        }

        // Delete from email_addresses
        let sql = "DELETE FROM email_addresses WHERE address = $1";
        match client.execute(sql, &[&address]).await {
            Ok(rows) => Ok(rows > 0),
            Err(e) => {
                error!("Failed to delete email address: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn list_email_addresses(&self) -> Result<Vec<EmailAddressInfo>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = r#"
            SELECT
                ea.address,
                ea.created_at,
                COALESCE((SELECT COUNT(*) FROM mail m WHERE m.recipients = ea.address), 0) as email_count
            FROM email_addresses ea
            ORDER BY ea.created_at DESC
        "#;
        match client.query(sql, &[]).await {
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

    // ============== ANALYTICS OPERATIONS ==============

    pub async fn increment_analytics(&self, event_type: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = r#"
            INSERT INTO analytics (event_type, event_count, last_updated)
            VALUES ($1, 1, now()::text)
            ON CONFLICT (event_type) DO UPDATE SET
                event_count = analytics.event_count + 1,
                last_updated = now()::text
        "#;
        match client.execute(sql, &[&event_type]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to increment analytics: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn get_analytics(&self) -> Result<Vec<AnalyticsRow>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT event_type, event_count, last_updated FROM analytics ORDER BY event_count DESC";
        match client.query(sql, &[]).await {
            Ok(rows) => {
                let analytics: Vec<AnalyticsRow> = rows
                    .into_iter()
                    .map(|row| AnalyticsRow {
                        event_type: row.get(0),
                        event_count: row.get(1),
                        last_updated: row.get(2),
                    })
                    .collect();
                Ok(analytics)
            }
            Err(e) => {
                error!("Failed to get analytics: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn get_total_stats(&self) -> Result<TotalStats, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;

        let email_addresses_count: i64 = client
            .query_one("SELECT COUNT(*) FROM email_addresses", &[])
            .await
            .map(|row| row.get(0))
            .unwrap_or(0);

        let total_emails_count: i64 = client
            .query_one("SELECT COUNT(*) FROM mail", &[])
            .await
            .map(|row| row.get(0))
            .unwrap_or(0);

        let total_webhooks_count: i64 = client
            .query_one("SELECT COUNT(*) FROM user_config WHERE web_hook_address IS NOT NULL", &[])
            .await
            .map(|row| row.get(0))
            .unwrap_or(0);

        Ok(TotalStats {
            total_email_addresses: email_addresses_count,
            total_emails_received: total_emails_count,
            total_webhooks_configured: total_webhooks_count,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsRow {
    pub event_type: String,
    pub event_count: i64,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotalStats {
    pub total_email_addresses: i64,
    pub total_emails_received: i64,
    pub total_webhooks_configured: i64,
}
