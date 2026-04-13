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
use tower_http::cors::{AllowHeaders, CorsLayer, AllowOrigin, AllowMethods};
use tracing::info;
use tracing_subscriber::fmt;

#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

async fn root() -> Json<ApiResponse<&'static str>> {
    Json(ApiResponse {
        success: true,
        data: Some("Temp Mail HTTP API is running"),
        error: None,
    })
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

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::exact("https://void.kreyon.in".parse().unwrap()))
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    let app = Router::new()
        .route("/", get(root))
        .route("/api/emails/:address", get(get_emails))
        .route("/api/emails/:address/:id", get(get_email))
        .route("/api/emails/:address/:id", delete(delete_email))
        .with_state(db)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("HTTP server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_success() {
        let response = ApiResponse {
            success: true,
            data: Some("test data"),
            error: None,
        };
        assert!(response.success);
        assert_eq!(response.data, Some("test data"));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<()> = ApiResponse {
            success: false,
            data: None,
            error: Some("Something went wrong".to_string()),
        };
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error, Some("Something went wrong".to_string()));
    }

    #[test]
    fn test_api_response_serde() {
        let response = ApiResponse {
            success: true,
            data: Some("test"),
            error: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"test\""));
    }

    #[tokio::test]
    async fn test_root_handler() {
        let response = root().await;
        assert!(response.success);
        assert_eq!(response.data, Some("Temp Mail HTTP API is running"));
    }

    #[test]
    fn test_address_parsing() {
        let path: (String, i64) = (String::from("test@example.com"), 123);
        assert_eq!(path.0, "test@example.com");
        assert_eq!(path.1, 123);
    }
}
