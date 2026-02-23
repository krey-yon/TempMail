use chrono::DateTime;
use std::{env, error::Error};
use tokio_postgres::{Client, NoTls};
use tracing::{error, info};

pub struct DatabaseClient {
    pub db: Client,
}

impl DatabaseClient {
    pub async fn connect() -> Result<Self, Box<dyn Error>> {
        let host = env::var("DB_HOST").expect("DB_HOST not set");
        let user = env::var("DB_USER").expect("DB_USER not set");
        let password = env::var("DB_PASSWORD").expect("DB_PASSWORD not set");
        let dbname = env::var("DB_NAME").expect("DB_NAME not set");
 
        let connection_string: String = format!(
            "host={} user={} password={} dbname={}",
            host, user, password, dbname
        );

        let (client, connection) = match tokio_postgres::connect(&connection_string, NoTls).await {
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

        let sql: &str = "
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
";

        if let Err(e) = client.batch_execute(sql).await {
            error!("Failed to execute initialization queries: {}", e);
            return Err(Box::new(e));
        }

        info!("Database initialized successfully");
        Ok(DatabaseClient { db: client })
    }

    pub async fn add_mail(&self, data: Email) -> Result<u64, Box<dyn Error>> {
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

    pub async fn delete_old_mail(&self) -> Result<u64, Box<dyn Error>> {
        let now: DateTime<chrono::Utc> = chrono::offset::Utc::now();
        let a_week_ago: DateTime<chrono::Utc> = now - chrono::Duration::days(7);
        let a_week_ago: String = a_week_ago.format("%Y-%m-%d %H:%M:%S%.3f").to_string();

        info!("Deleting old mail from before {a_week_ago}");
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

    pub async fn get_mails_by_recipient(&self, recipient: &str) -> Result<Vec<MailRow>, Box<dyn Error>> {
        let sql = "SELECT id, date, sender, recipients, data FROM mail WHERE recipients = $1 ORDER BY date DESC";
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

    pub async fn get_mail_by_id(&self, id: i64) -> Result<Option<MailRow>, Box<dyn Error>> {
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

    pub async fn delete_mail(&self, id: i64) -> Result<u64, Box<dyn Error>> {
        let sql = "DELETE FROM mail WHERE id = $1";
        match self.db.execute(sql, &[&id]).await {
            Ok(rows) => Ok(rows),
            Err(e) => {
                error!("Failed to delete mail: {}", e);
                Err(Box::new(e))
            }
        }
    }
}



#[derive(Default, Clone, Debug)]
pub struct Email {
    pub sender: String,
    pub recipients: Vec<String>,
    pub content: String,
    pub size: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MailRow {
    pub id: i64,
    pub date: String,
    pub sender: String,
    pub recipients: String,
    pub data: String,
}
