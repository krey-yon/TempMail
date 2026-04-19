use crate::database::DatabaseClient;
use std::error::Error;
use std::sync::Arc;
use tracing::{error, info};

#[derive(Debug, Clone)]
pub struct AddressLimits {
    pub address: String,
    pub limit: i32,
    pub completed: i32,
}

impl AddressLimits {
    pub async fn get_details_for_address(
        db: &Arc<DatabaseClient>,
        address: &str,
    ) -> Result<Option<Self>, Box<dyn Error + Send + Sync>> {
        let sql = "SELECT address, quota_limit, completed FROM quota WHERE address = $1";
        match db.db.query_one(sql, &[&address]).await {
            Ok(row) => Ok(Some(AddressLimits {
                address: row.get(0),
                limit: row.get(1),
                completed: row.get(2),
            })),
            Err(e) if e.to_string().contains("no rows returned") => Ok(None),
            Err(e) => {
                error!("Failed to get quota details: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn increment(db: &Arc<DatabaseClient>, address: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sql = "UPDATE quota SET completed = completed + 1 WHERE address = $1";
        match db.db.execute(sql, &[&address]).await {
            Ok(_) => {
                info!("Incremented quota for {}", address);
                Ok(())
            }
            Err(e) => {
                error!("Failed to increment quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn create_default(
        db: &Arc<DatabaseClient>,
        address: &str,
        limit: i32,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let sql = "INSERT INTO quota (address, quota_limit, completed) VALUES ($1, $2, 0) ON CONFLICT (address) DO NOTHING";
        match db.db.execute(sql, &[&address, &limit]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Failed to create quota: {}", e);
                Err(Box::new(e))
            }
        }
    }

    pub async fn check_and_increment(
        db: &Arc<DatabaseClient>,
        address: &str,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        if let Some(limits) = Self::get_details_for_address(db, address).await? {
            if limits.completed >= limits.limit {
                info!("Quota exceeded for {}", address);
                return Ok(false);
            }
            Self::increment(db, address).await?;
            return Ok(true);
        }
        Ok(true)
    }
}
