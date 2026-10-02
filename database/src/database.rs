use chrono::DateTime;
use deadpool_postgres::{Manager, Pool, Runtime};
use rustls::ClientConfig;
use rustls_native_certs::load_native_certs;
use serde::{Deserialize, Serialize};
use std::{env, error::Error};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{error, info};

fn token_eq(stored: &str, presented: &str) -> bool {
    if stored.len() != presented.len() {
        return false;
    }
    stored
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailRecipient {
    pub address: String,
    pub mailbox_id: String,
}

#[derive(Debug)]
pub enum MailDeliveryError {
    MailboxUnavailable,
    QuotaExceeded,
}

impl std::fmt::Display for MailDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MailboxUnavailable => "Mailbox unavailable",
            Self::QuotaExceeded => "Mailbox quota exceeded",
        })
    }
}

impl Error for MailDeliveryError {}

// All lifecycle mutations lock the address, including when no mailbox exists.
// This prevents creation, expiry, and SMTP delivery from crossing ownerships.
async fn lock_address(
    tx: &tokio_postgres::Transaction<'_>,
    address: &str,
) -> Result<(), tokio_postgres::Error> {
    tx.query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&address]).await?;
    Ok(())
}

async fn remove_mailbox(
    tx: &tokio_postgres::Transaction<'_>,
    address: &str,
) -> Result<u64, tokio_postgres::Error> {
    tx.execute("DELETE FROM mail WHERE recipients = $1", &[&address]).await?;
    tx.execute("DELETE FROM user_config WHERE mail = $1 OR address = $1", &[&address]).await?;
    tx.execute("DELETE FROM quota WHERE address = $1", &[&address]).await?;
    tx.execute("DELETE FROM email_registry WHERE username = split_part($1, '@', 1)", &[&address]).await?;
    tx.execute("DELETE FROM email_addresses WHERE address = $1", &[&address]).await
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddressInfo {
    pub address: String,
    pub created_at: Option<String>,
    pub email_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailRegistryEntry {
    pub username: String,
    pub created_at: Option<String>,
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
        pg_config.ssl_mode(match env::var("DB_SSLMODE").as_deref().unwrap_or("prefer") {
            "disable" => tokio_postgres::config::SslMode::Disable,
            "prefer" => tokio_postgres::config::SslMode::Prefer,
            "require" => tokio_postgres::config::SslMode::Require,
            _ => return Err("DB_SSLMODE must be disable, prefer, or require".into()),
        });

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

        // Create extension separately (ignore errors if already exists)
        let _ = client.execute("CREATE EXTENSION IF NOT EXISTS \"pgcrypto\"", &[]).await;

        // Create tables
        let sql: &str = r#"
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
            EXCEPTION WHEN OTHERS THEN
                RAISE NOTICE 'FK constraint may already exist: %', SQLERRM;
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
                event_type TEXT NOT NULL UNIQUE,
                event_count BIGINT NOT NULL DEFAULT 0,
                last_updated TEXT NOT NULL DEFAULT (now()::text)
            );
            CREATE INDEX IF NOT EXISTS analytics_event_type_idx ON analytics(event_type);

            CREATE TABLE IF NOT EXISTS email_registry (
                id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                username TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (now()::text)
            );
            CREATE INDEX IF NOT EXISTS email_registry_username_idx ON email_registry(username);
        "#;

        if let Err(e) = client.batch_execute(sql).await {
            error!("Failed to execute initialization queries: {}", e);
            return Err(Box::new(e));
        }

        let token_sql = r#"
            ALTER TABLE email_addresses ADD COLUMN IF NOT EXISTS access_token TEXT UNIQUE;
            UPDATE email_addresses
            SET access_token = encode(gen_random_bytes(32), 'hex')
            WHERE access_token IS NULL;
        "#;
        if let Err(e) = client.batch_execute(token_sql).await {
            error!("Failed to add mailbox access_token column: {}", e);
            return Err(Box::new(e));
        }

        client.batch_execute(include_str!("../migrations/004_reusable_usernames.sql")).await?;

        info!("Database initialized successfully with connection pooling");
        Ok(DatabaseClient { pool })
    }

    // ============== MAIL OPERATIONS ==============

    pub async fn get_active_mailbox(&self, address: &str) -> Result<Option<MailRecipient>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let row = client.query_opt(
            "SELECT id::text FROM email_addresses WHERE address = $1 AND created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'",
            &[&address],
        ).await?;
        Ok(row.map(|row| MailRecipient { address: address.to_string(), mailbox_id: row.get(0) }))
    }

    // Save all recipients before SMTP acknowledges DATA. The captured mailbox
    // IDs ensure mail cannot be delivered to a replacement owner mid-transaction.
    pub async fn deliver_mail(
        &self,
        data: Email,
        recipients: &[MailRecipient],
    ) -> Result<Vec<(String, Option<String>)>, Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let mut recipients = recipients.to_vec();
        recipients.sort_by(|a, b| a.address.cmp(&b.address));
        recipients.dedup_by(|a, b| a.address == b.address);
        let mut notifications = Vec::new();
        for recipient in &recipients {
            lock_address(&tx, &recipient.address).await?;
            let active = tx.query_opt(
                "SELECT id FROM email_addresses WHERE address = $1 AND id::text = $2 AND created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'",
                &[&recipient.address, &recipient.mailbox_id],
            ).await?;
            if active.is_none() {
                return Err(Box::new(MailDeliveryError::MailboxUnavailable));
            }
            let quota = tx.execute(
                "UPDATE quota SET completed = completed + 1 WHERE address = $1 AND completed < quota_limit",
                &[&recipient.address],
            ).await?;
            if quota == 0 {
                return Err(Box::new(MailDeliveryError::QuotaExceeded));
            }
            tx.execute(
                "INSERT INTO mail (date, sender, recipients, data) VALUES ((clock_timestamp() AT TIME ZONE 'UTC')::text, $1, $2, $3)",
                &[&data.sender, &recipient.address, &data.content],
            ).await?;
            let webhook = tx.query_opt(
                "SELECT web_hook_address FROM user_config WHERE mail = $1",
                &[&recipient.address],
            ).await?.and_then(|row| row.get::<_, Option<String>>(0));
            notifications.push((recipient.address.clone(), webhook));
        }
        let count = recipients.len() as i64;
        tx.execute(
            "INSERT INTO analytics (event_type, event_count, last_updated) VALUES ('emails_received', $1, now()::text) ON CONFLICT (event_type) DO UPDATE SET event_count = analytics.event_count + EXCLUDED.event_count, last_updated = EXCLUDED.last_updated",
            &[&count],
        ).await?;
        tx.commit().await?;
        Ok(notifications)
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
        token: &str,
    ) -> Result<Vec<MailRow>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let normalized_recipient = recipient.trim_start_matches('<').trim_end_matches('>');
        let sql =
            "SELECT m.id::text, m.date, m.sender, m.recipients, m.data FROM mail m JOIN email_addresses ea ON ea.address = m.recipients WHERE ea.address = $1 AND ea.access_token = $2 AND ea.created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours' ORDER BY m.date DESC";
        match client.query(sql, &[&normalized_recipient, &token]).await {
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

    pub async fn get_mail_by_id(&self, id: &str, address: &str, token: &str) -> Result<Option<MailRow>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT m.id::text, m.date, m.sender, m.recipients, m.data FROM mail m JOIN email_addresses ea ON ea.address = m.recipients WHERE m.id::text = $1 AND ea.address = $2 AND ea.access_token = $3 AND ea.created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'";
        match client.query_opt(sql, &[&id, &address, &token]).await {
            Ok(Some(row)) => Ok(Some(MailRow {
                id: row.get(0),
                date: row.get(1),
                sender: row.get(2),
                recipients: row.get(3),
                data: row.get(4),
            })),
            Ok(None) => Ok(None),
            Err(e) => {
                error!("Failed to get mail by id: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_mail(&self, id: &str, address: &str, token: &str) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "DELETE FROM mail m USING email_addresses ea WHERE m.recipients = ea.address AND m.id::text = $1 AND ea.address = $2 AND ea.access_token = $3 AND ea.created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'";
        match client.execute(sql, &[&id, &address, &token]).await {
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
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        lock_address(&tx, &address).await?;

        let existing = tx.query_opt(
            "SELECT created_at::timestamp <= (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours' FROM email_addresses WHERE address = $1",
            &[&address],
        ).await?;
        if let Some(row) = existing {
            if !row.get::<_, bool>(0) {
                return Err(format!("Username '{}' is already taken. Please choose a different username.", username).into());
            }
        }
        // Also clear any orphaned data left by older versions. This never runs
        // for an active mailbox and is rolled back if creation fails.
        remove_mailbox(&tx, &address).await?;
        let row = tx.query_one(
            "INSERT INTO email_addresses (address, created_at, access_token) VALUES ($1, (clock_timestamp() AT TIME ZONE 'UTC')::text, encode(gen_random_bytes(32), 'hex')) RETURNING created_at, access_token",
            &[&address],
        ).await?;
        let created_at: String = row.get(0);
        let access_token: String = row.get(1);
        tx.execute(
            "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, 1000, 0)",
            &[&address],
        ).await?;
        tx.execute(
            "INSERT INTO email_registry (username, created_at) VALUES ($1, $2) ON CONFLICT (username) DO UPDATE SET created_at = EXCLUDED.created_at",
            &[&username, &created_at],
        ).await?;
        tx.execute(
            "INSERT INTO analytics (event_type, event_count, last_updated) VALUES ('email_address_created', 1, now()::text) ON CONFLICT (event_type) DO UPDATE SET event_count = analytics.event_count + 1, last_updated = EXCLUDED.last_updated",
            &[],
        ).await?;
        tx.commit().await?;
        Ok(EmailAddress { address, created_at: Some(created_at), access_token: Some(access_token) })
    }

    pub async fn mailbox_token_matches(
        &self,
        address: &str,
        presented: &str,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT access_token FROM email_addresses WHERE address = $1 AND created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'";
        match client.query_opt(sql, &[&address]).await {
            Ok(Some(row)) => {
                let stored: Option<String> = row.get(0);
                Ok(stored
                    .as_deref()
                    .map(|s| token_eq(s, presented))
                    .unwrap_or(false))
            }
            Ok(None) => Ok(false),
            Err(e) => {
                error!("Failed to read mailbox token: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_email_address(&self, address: &str, token: &str) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        lock_address(&tx, address).await?;
        let row = tx.query_opt(
            "SELECT access_token FROM email_addresses WHERE address = $1 AND created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'",
            &[&address],
        ).await?;
        let valid = row.and_then(|row| row.get::<_, Option<String>>(0))
            .is_some_and(|stored| token_eq(&stored, token));
        if !valid {
            return Ok(false);
        }
        let rows = remove_mailbox(&tx, address).await?;
        tx.commit().await?;
        Ok(rows > 0)
    }

    pub async fn delete_old_email_addresses(&self) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let mut client = self.pool.get().await?;
        let tx = client.transaction().await?;
        let rows = tx.query(
            "SELECT address FROM email_addresses WHERE created_at::timestamp <= (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours' ORDER BY address",
            &[],
        ).await?;
        let mut deleted_count = 0;
        for row in rows {
            let address: String = row.get(0);
            lock_address(&tx, &address).await?;
            // A creator may have reclaimed it while cleanup waited for the lock.
            let expired = tx.query_opt(
                "SELECT id FROM email_addresses WHERE address = $1 AND created_at::timestamp <= (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'",
                &[&address],
            ).await?;
            if expired.is_some() {
                deleted_count += remove_mailbox(&tx, &address).await?;
            }
        }
        tx.commit().await?;
        info!("Deleted {} expired email addresses", deleted_count);
        Ok(deleted_count)
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

    pub async fn list_registered_usernames(&self) -> Result<Vec<EmailRegistryEntry>, Box<dyn Error + Send + Sync>> {
        let client = self.pool.get().await?;
        let sql = "SELECT username, created_at FROM email_registry ORDER BY created_at DESC";
        match client.query(sql, &[]).await {
            Ok(rows) => {
                let entries: Vec<EmailRegistryEntry> = rows
                    .into_iter()
                    .map(|row| EmailRegistryEntry {
                        username: row.get(0),
                        created_at: row.get(1),
                    })
                    .collect();
                Ok(entries)
            }
            Err(e) => {
                error!("Failed to list registered usernames: {}", e);
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
        let row = client.query_one(
            "SELECT (SELECT COUNT(*) FROM email_addresses WHERE created_at::timestamp > (clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours'), (SELECT COUNT(*) FROM mail), (SELECT COUNT(*) FROM user_config WHERE web_hook_address IS NOT NULL), COALESCE((SELECT event_count FROM analytics WHERE event_type = 'email_address_created'), 0)",
            &[],
        ).await?;
        Ok(TotalStats {
            total_email_addresses: row.get(0),
            total_emails_received: row.get(1),
            total_webhooks_configured: row.get(2),
            total_addresses_created: row.get(3),
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
    pub total_addresses_created: i64,
}
