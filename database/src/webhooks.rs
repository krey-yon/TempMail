use crate::database::DatabaseClient;
use std::error::Error;
use std::sync::Arc;
use tracing::{error, info};

pub struct Webhooks;

impl Webhooks {
    pub async fn get_webhook_address_for_mail(
        db: &Arc<DatabaseClient>,
        mail: &str,
    ) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let sql = "SELECT web_hook_address FROM user_config WHERE mail = $1";
        match client.query_one(sql, &[&mail]).await {
            Ok(row) => Ok(row.get(0)),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get webhook address: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn set_webhook(
        db: &Arc<DatabaseClient>,
        mail: &str,
        webhook_url: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let client = db.pool.get().await?;
        let address = mail.split('@').next().unwrap_or(mail);
        let sql = "INSERT INTO user_config (mail, address, web_hook_address) VALUES ($1, $2, $3) ON CONFLICT (mail) DO UPDATE SET web_hook_address = $3";
        match client.execute(sql, &[&mail, &address, &webhook_url]).await {
            Ok(_) => {
                info!("Set webhook for {} to {}", mail, webhook_url);
                Ok(())
            }
            Err(e) => {
                error!("Failed to set webhook: {}", e);
                Err(Box::new(e))
            }
        }
    }
}
