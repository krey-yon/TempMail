use axum::{
    routing::{get, delete},
    Router,
    extract::{Path, State},
    response::Json,
};
use database::database::DatabaseClient;
use dotenv::dotenv;
use serde::Serialize;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::fmt;

#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

async fn get_emails(
    Path(address): Path<String>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<Vec<database::database::MailRow>>> {
    match db.get_mails_by_recipient(&address).await {
        Ok(mails) => Json(ApiResponse {
            success: true,
            data: Some(mails),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn get_email(
    Path((address, id)): Path<(String, i64)>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<database::database::MailRow>> {
    match db.get_mail_by_id(id).await {
        Ok(Some(mail)) => {
            if mail.recipients == address {
                Json(ApiResponse {
                    success: true,
                    data: Some(mail),
                    error: None,
                })
            } else {
                Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some("Email not found for this address".to_string()),
                })
            }
        }
        Ok(None) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some("Email not found".to_string()),
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn delete_email(
    Path((address, id)): Path<(String, i64)>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<()>> {
    match db.get_mail_by_id(id).await {
        Ok(Some(mail)) => {
            if mail.recipients != address {
                return Json(ApiResponse {
                    success: false,
                    data: None,
                    error: Some("Email not found for this address".to_string()),
                });
            }
        }
        Ok(None) => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some("Email not found".to_string()),
            });
        }
        Err(e) => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some(e.to_string()),
            });
        }
    }

    match db.delete_mail(id).await {
        Ok(_) => Json(ApiResponse {
            success: true,
            data: Some(()),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    fmt::init();

    let db = Arc::new(DatabaseClient::connect().await.unwrap());

    let app = Router::new()
        .route("/api/emails/:address", get(get_emails))
        .route("/api/emails/:address/:id", get(get_email))
        .route("/api/emails/:address/:id", delete(delete_email))
        .with_state(db);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("HTTP server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}
