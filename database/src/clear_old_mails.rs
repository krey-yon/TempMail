use crate::database::DatabaseClient;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

pub fn clear_old_mails(interval: Duration) {
    tokio::spawn(async move {
        let mut interval_timer = time::interval(interval);

        loop {
            interval_timer.tick().await;
            info!("Starting automatic email cleanup");

            match DatabaseClient::connect().await {
                Ok(db) => {
                    match db.delete_old_mail().await {
                        Ok(count) => info!("Deleted {} old emails", count),
                        Err(e) => error!("Cleanup failed: {}", e),
                    }
                }
                Err(e) => error!("Failed to connect to database for cleanup: {}", e),
            }
        }
    });
}
