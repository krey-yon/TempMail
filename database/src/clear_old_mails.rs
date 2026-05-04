use crate::database::DatabaseClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

pub fn clear_old_mails(db: Arc<DatabaseClient>, interval: Duration) {
    tokio::spawn(async move {
        let mut interval_timer = time::interval(interval);

        loop {
            interval_timer.tick().await;
            info!("Starting automatic email cleanup");

            match db.delete_old_mail().await {
                Ok(count) => info!("Deleted {} old emails", count),
                Err(e) => error!("Cleanup failed: {}", e),
            }

            match db.delete_old_email_addresses().await {
                Ok(count) => info!("Deleted {} old email addresses", count),
                Err(e) => error!("Address cleanup failed: {}", e),
            }
        }
    });
}
