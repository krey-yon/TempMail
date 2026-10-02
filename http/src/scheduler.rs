use database::database::DatabaseClient;
use std::error::Error;
use std::sync::Arc;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

pub async fn start_cleanup_scheduler(db: Arc<DatabaseClient>) -> Result<(), Box<dyn Error>> {
    let scheduler = JobScheduler::new().await?;

    // Sweep expired records every minute; access and creation enforce the 24-hour deadline.
    let job = Job::new_async("0 * * * * *", move |_uuid, _l| {
        let db = db.clone();
        Box::pin(async move {
            info!("Starting automatic email cleanup");
            match db.delete_old_mail().await {
                Ok(count) => info!("Deleted {} old emails", count),
                Err(e) => error!("Cleanup failed: {}", e),
            }
            match db.delete_old_email_addresses().await {
                Ok(count) => info!("Deleted {} old email addresses", count),
                Err(e) => error!("Address cleanup failed: {}", e),
            }
        })
    })?;

    scheduler.add(job).await?;
    scheduler.start().await?;

    info!("Cleanup scheduler started (runs every minute)");
    Ok(())
}
