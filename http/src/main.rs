mod types;
mod validation;
mod scheduler;
mod webhook;

#[cfg(test)]
mod validation_properties;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use database::database::{AnalyticsRow, DatabaseClient, EmailAddress, EmailAddressInfo, MailRow};
use dotenv::dotenv;
use std::sync::Arc;
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer,
};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing::{error, info, warn};
use tracing_subscriber::fmt;
use types::{ApiResponse, CreateEmailRequest, DeleteEmailResponse};
use validation::UsernameValidator;

async fn root() -> Json<ApiResponse<&'static str>> {
    Json(ApiResponse::success("Temp Mail HTTP API is running"))
}

async fn create_email(
    State(db): State<Arc<DatabaseClient>>,
    Json(payload): Json<CreateEmailRequest>,
) -> Response {
    info!("Creating email address for username: {}", payload.username);

    // Validate username
    let validated_username = match UsernameValidator::validate(&payload.username) {
        Ok(username) => username,
        Err(e) => {
            warn!("Username validation failed: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<EmailAddress>::error(e.to_string())),
            )
                .into_response();
        }
    };

    // Create email address
    match db.create_email_address(&validated_username).await {
        Ok(email_address) => {
            info!("Email address created: {}", email_address.address);
            // Track analytics
            let _ = db.increment_analytics("email_address_created").await;
            (
                StatusCode::CREATED,
                Json(ApiResponse::success(email_address)),
            )
                .into_response()
        }
        Err(e) if e.to_string().contains("already exists") => {
            warn!("Duplicate email address attempt: {}", validated_username);
            // Track failed attempt
            let _ = db.increment_analytics("duplicate_address_attempt").await;
            (
                StatusCode::CONFLICT,
                Json(ApiResponse::<EmailAddress>::error(
                    "Email address already exists".to_string(),
                )),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to create email address: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<EmailAddress>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn delete_email_address(
    Path(address): Path<String>,
    State(db): State<Arc<DatabaseClient>>,
) -> Response {
    info!("Deleting email address: {}", address);

    // Validate email format
    if let Err(e) = UsernameValidator::validate_email_format(&address) {
        warn!("Invalid email format: {}", address);
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<DeleteEmailResponse>::error(e.to_string())),
        )
            .into_response();
    }

    // Delete email address
    match db.delete_email_address(&address).await {
        Ok(true) => {
            info!("Email address deleted: {}", address);
            (
                StatusCode::OK,
                Json(ApiResponse::success(DeleteEmailResponse {
                    message: "Email address deleted successfully".to_string(),
                    address: address.clone(),
                })),
            )
                .into_response()
        }
        Ok(false) => {
            warn!("Email address not found: {}", address);
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<DeleteEmailResponse>::error(
                    "Email address not found".to_string(),
                )),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to delete email address: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<DeleteEmailResponse>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn list_emails(State(db): State<Arc<DatabaseClient>>) -> Response {
    info!("Listing all email addresses");

    match db.list_email_addresses().await {
        Ok(addresses) => {
            info!("Found {} email addresses", addresses.len());
            (StatusCode::OK, Json(ApiResponse::success(addresses))).into_response()
        }
        Err(e) => {
            error!("Failed to list email addresses: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<EmailAddressInfo>>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

#[derive(serde::Serialize)]
struct StatsResponse {
    total_addresses: i64,
    total_emails: i64,
    total_webhooks: i64,
    events: Vec<AnalyticsRow>,
}

async fn get_stats(State(db): State<Arc<DatabaseClient>>) -> Response {
    info!("Getting statistics");

    match db.get_total_stats().await {
        Ok(stats) => {
            let events = db.get_analytics().await.unwrap_or_default();
            let response = StatsResponse {
                total_addresses: stats.total_email_addresses,
                total_emails: stats.total_emails_received,
                total_webhooks: stats.total_webhooks_configured,
                events,
            };
            (StatusCode::OK, Json(ApiResponse::success(response))).into_response()
        }
        Err(e) => {
            error!("Failed to get stats: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<StatsResponse>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn get_emails(
    Path(address): Path<String>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<Vec<MailRow>>> {
    match db.get_mails_by_recipient(&address).await {
        Ok(mails) => {
            // Track email fetch
            let _ = db.increment_analytics("emails_fetched").await;
            Json(ApiResponse::success(mails))
        }
        Err(e) => {
            error!("Failed to get emails: {}", e);
            Json(ApiResponse::error(e.to_string()))
        }
    }
}

async fn get_email(
    Path((address, id)): Path<(String, String)>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<MailRow>> {
    match db.get_mail_by_id(&id).await {
        Ok(Some(mail)) => {
            if mail.recipients == address {
                Json(ApiResponse::success(mail))
            } else {
                Json(ApiResponse::error(
                    "Email not found for this address".to_string(),
                ))
            }
        }
        Ok(None) => Json(ApiResponse::error("Email not found".to_string())),
        Err(e) => {
            error!("Failed to get email: {}", e);
            Json(ApiResponse::error(e.to_string()))
        }
    }
}

async fn delete_email(
    Path((address, id)): Path<(String, String)>,
    State(db): State<Arc<DatabaseClient>>,
) -> Json<ApiResponse<()>> {
    match db.get_mail_by_id(&id).await {
        Ok(Some(mail)) => {
            if mail.recipients != address {
                return Json(ApiResponse::error(
                    "Email not found for this address".to_string(),
                ));
            }
        }
        Ok(None) => {
            return Json(ApiResponse::error("Email not found".to_string()));
        }
        Err(e) => {
            error!("Failed to get email: {}", e);
            return Json(ApiResponse::error(e.to_string()));
        }
    }

    match db.delete_mail(&id).await {
        Ok(_) => Json(ApiResponse::success(())),
        Err(e) => {
            error!("Failed to delete email: {}", e);
            Json(ApiResponse::error(e.to_string()))
        }
    }
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    fmt::init();

    let db = Arc::new(DatabaseClient::connect().await.unwrap());

    // Start cleanup scheduler
    if let Err(e) = scheduler::start_cleanup_scheduler(db.clone()).await {
        error!("Failed to start cleanup scheduler: {}", e);
    }

    // Configure rate limiter: 100 requests per minute per IP (DDoS protection)
    // Note: SmartIpKeyExtractor requires proxy headers (X-Forwarded-For, X-Real-IP)
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(100) // 100 requests per second
            .burst_size(150) // Allow burst of up to 150 requests
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .unwrap(),
    );

    let governor_layer = GovernorLayer {
        config: governor_conf,
    };

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let origin_str = origin.to_str().unwrap_or("");
            origin_str == "https://www.xelio.me"
                || origin_str == "http://localhost:3000"
                || origin_str.starts_with("http://localhost:")
        }))
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    let app = Router::new()
        .route("/", get(root))
        .route("/api/emails", post(create_email))
        .route("/api/emails", get(list_emails))
        .route("/api/emails/:address", delete(delete_email_address))
        .route("/api/emails/:address", get(get_emails))
        .route("/api/emails/:address/:id", get(get_email))
        .route("/api/emails/:address/:id", delete(delete_email))
        .route("/api/stats", get(get_stats))
        .layer(governor_layer)
        .with_state(db)
        .layer(cors);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();
    info!("HTTP server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_success() {
        let response = ApiResponse::success("test data");
        assert!(response.success);
        assert_eq!(response.data, Some("test data"));
        assert!(response.error.is_none());
    }

    #[test]
    fn test_api_response_error() {
        let response: ApiResponse<()> = ApiResponse::error("Something went wrong".to_string());
        assert!(!response.success);
        assert!(response.data.is_none());
        assert_eq!(response.error, Some("Something went wrong".to_string()));
    }

    #[tokio::test]
    async fn test_root_handler() {
        let response = root().await;
        assert!(response.success);
        assert_eq!(response.data, Some("Temp Mail HTTP API is running"));
    }
}
