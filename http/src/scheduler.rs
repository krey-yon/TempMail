use database::database::DatabaseClient;
use std::error::Error;
use std::sync::Arc;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info};

pub async fn start_cleanup_scheduler(db: Arc<DatabaseClient>) -> Result<(), Box<dyn Error>> {
    let scheduler = JobScheduler::new().await?;

    // Run every day at 2 AM UTC
    let job = Job::new_async("0 0 2 * * *", move |_uuid, _l| {
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

    info!("Cleanup scheduler started (runs daily at 2 AM UTC)");
    Ok(())
}
