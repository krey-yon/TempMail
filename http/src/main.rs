mod access_log;
mod admin_panel;
mod alerts;
mod pages;
mod scheduler;
mod turnstile;
mod types;
mod validation;
mod webhook;

#[cfg(test)]
mod validation_properties;

use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware,
    response::{Html, IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use database::database::{
    AnalyticsRow, DatabaseClient, EmailAddress, EmailAddressInfo, EmailRegistryEntry, MailRow,
};
use dotenv::dotenv;
use std::env;
use std::sync::Arc;
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer,
};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing::{error, info, warn};
use tracing_subscriber::fmt;
use types::{ApiResponse, CreateEmailRequest, DeleteEmailResponse};
use validation::UsernameValidator;

fn mailbox_token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("x-mailbox-token") {
        return value.to_str().ok().map(|s| s.to_string());
    }
    let auth = headers.get("authorization")?.to_str().ok()?;
    let bearer = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))?;
    Some(bearer.to_string())
}

pub(crate) fn admin_authorized(headers: &HeaderMap) -> bool {
    let Ok(expected) = env::var("ADMIN_TOKEN") else {
        return false;
    };
    if expected.is_empty() {
        return false;
    }
    let presented = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

async fn require_mailbox_token(
    db: &Arc<DatabaseClient>,
    address: &str,
    headers: &HeaderMap,
) -> Option<Response> {
    let Some(token) = mailbox_token_from_headers(headers) else {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error(
                    "Mailbox token required".to_string(),
                )),
            )
                .into_response(),
        );
    };
    match db.mailbox_token_matches(address, &token).await {
        Ok(true) => None,
        Ok(false) => Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error(
                    "Invalid mailbox token".to_string(),
                )),
            )
                .into_response(),
        ),
        Err(e) => {
            error!("Failed to verify mailbox token: {}", e);
            Some(
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse::<()>::error(
                        "Internal server error".to_string(),
                    )),
                )
                    .into_response(),
            )
        }
    }
}

/// Mailbox contents must never be retained by a browser, proxy, or CDN: the
/// token is the only credential, and a cached body outlives it.
async fn no_store(request: axum::extract::Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn root() -> Json<ApiResponse<&'static str>> {
    Json(ApiResponse::success("Temp Mail HTTP API is running"))
}

/// Records one mailbox-creation attempt for durable attribution.
///
/// The in-memory access log is a live view for the panel; this row survives
/// restarts and deploys, which is what makes it useful if the abuse ever has
/// to be reported. Failures are logged and ignored: forensics must never break
/// a request.
fn spawn_access_event(
    db: Arc<DatabaseClient>,
    ip: String,
    user_agent: String,
    origin: Option<String>,
    status: i32,
) {
    tokio::spawn(async move {
        if let Err(error) = db
            .log_access_event(&ip, &user_agent, origin.as_deref(), "/api/emails", status)
            .await
        {
            warn!("Failed to persist access event: {}", error);
        }
    });
}

async fn create_email(
    State(db): State<Arc<DatabaseClient>>,
    Extension(turnstile): Extension<Arc<turnstile::Turnstile>>,
    headers: HeaderMap,
    Json(payload): Json<CreateEmailRequest>,
) -> Response {
    let ip = access_log::client_ip(&headers);
    let user_agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(512)
        .collect::<String>();
    let origin = headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(512).collect::<String>());

    let response = create_email_inner(db.clone(), turnstile, &headers, payload, &ip).await;
    spawn_access_event(
        db,
        ip,
        user_agent,
        origin,
        response.status().as_u16() as i32,
    );
    response
}

async fn create_email_inner(
    db: Arc<DatabaseClient>,
    turnstile: Arc<turnstile::Turnstile>,
    headers: &HeaderMap,
    payload: CreateEmailRequest,
    ip: &str,
) -> Response {
    info!("Creating email address for username: {}", payload.username);

    // Validate username first: spending a Turnstile round trip on a request
    // that cannot succeed would only add latency.
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

    // Human verification. Disabled by default so the rollout is one env change
    // and the rollback is the same env change.
    let header_token = headers
        .get("cf-turnstile-response")
        .and_then(|value| value.to_str().ok());
    let token = payload
        .turnstile_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(header_token);

    if let Err(error) = turnstile.verify(token, ip).await {
        use turnstile::TurnstileError;
        let (status, code, message) = match &error {
            TurnstileError::Missing => (
                StatusCode::FORBIDDEN,
                "turnstile_missing",
                "Human verification is required. Reload the page and try again.",
            ),
            TurnstileError::Rejected(codes) => {
                warn!(
                    ip = %ip,
                    reason = %codes.join(","),
                    "Turnstile rejected a mailbox creation"
                );
                (
                    StatusCode::FORBIDDEN,
                    "turnstile_failed",
                    "Human verification failed. Reload the page and try again.",
                )
            }
            TurnstileError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "turnstile_unavailable",
                "Verification is temporarily unavailable. Please try again shortly.",
            ),
        };
        return (
            status,
            Json(ApiResponse::<EmailAddress>::error_with_code(message, code)),
        )
            .into_response();
    }

    // Create email address
    match db.create_email_address(&validated_username).await {
        Ok(email_address) => {
            info!("Email address created: {}", email_address.address);
            (
                StatusCode::CREATED,
                Json(ApiResponse::success(email_address)),
            )
                .into_response()
        }
        Err(e) => {
            let err_msg = e.to_string();
            error!("Failed to create email address: {}", err_msg);

            // Check for specific error types and return appropriate status codes
            let (status, message) = if err_msg.contains("already exists")
                || err_msg.contains("already taken")
                || err_msg.contains("duplicate key")
            {
                (
                    StatusCode::CONFLICT,
                    format!(
                        "Username '{}' is already taken. Please choose a different username.",
                        validated_username
                    ),
                )
            } else if err_msg.contains("server unavailable") || err_msg.contains("connection") {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Database server is temporarily unavailable. Please try again later."
                        .to_string(),
                )
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, err_msg)
            };

            (status, Json(ApiResponse::<EmailAddress>::error(message))).into_response()
        }
    }
}

async fn delete_email_address(
    Path(address): Path<String>,
    State(db): State<Arc<DatabaseClient>>,
    headers: HeaderMap,
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

    if let Some(denied) = require_mailbox_token(&db, &address, &headers).await {
        return denied;
    }

    // Delete email address
    match db
        .delete_email_address(
            &address,
            &mailbox_token_from_headers(&headers).unwrap_or_default(),
        )
        .await
    {
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

async fn list_emails(State(db): State<Arc<DatabaseClient>>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<Vec<EmailAddressInfo>>::error(
                "Admin token required".to_string(),
            )),
        )
            .into_response();
    }
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

async fn list_registry(State(db): State<Arc<DatabaseClient>>, headers: HeaderMap) -> Response {
    if !admin_authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<Vec<EmailRegistryEntry>>::error(
                "Admin token required".to_string(),
            )),
        )
            .into_response();
    }
    info!("Listing registered usernames");

    match db.list_registered_usernames().await {
        Ok(entries) => {
            info!("Found {} registered usernames", entries.len());
            (StatusCode::OK, Json(ApiResponse::success(entries))).into_response()
        }
        Err(e) => {
            error!("Failed to list registered usernames: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<EmailRegistryEntry>>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

#[derive(serde::Serialize)]
struct StatsResponse {
    total_addresses_created: i64,
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
                total_addresses_created: stats.total_addresses_created,
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
    headers: HeaderMap,
) -> Response {
    if let Some(denied) = require_mailbox_token(&db, &address, &headers).await {
        return denied;
    }
    match db
        .get_mails_by_recipient(
            &address,
            &mailbox_token_from_headers(&headers).unwrap_or_default(),
        )
        .await
    {
        Ok(mails) => {
            // Track email fetch
            let _ = db.increment_analytics("emails_fetched").await;
            (StatusCode::OK, Json(ApiResponse::success(mails))).into_response()
        }
        Err(e) => {
            error!("Failed to get emails: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<MailRow>>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn get_email(
    Path((address, id)): Path<(String, String)>,
    State(db): State<Arc<DatabaseClient>>,
    headers: HeaderMap,
) -> Response {
    if let Some(denied) = require_mailbox_token(&db, &address, &headers).await {
        return denied;
    }
    match db
        .get_mail_by_id(
            &id,
            &address,
            &mailbox_token_from_headers(&headers).unwrap_or_default(),
        )
        .await
    {
        Ok(Some(mail)) => {
            if mail.recipients == address {
                (StatusCode::OK, Json(ApiResponse::success(mail))).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(ApiResponse::<MailRow>::error(
                        "Email not found for this address".to_string(),
                    )),
                )
                    .into_response()
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<MailRow>::error("Email not found".to_string())),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to get email: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<MailRow>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn delete_email(
    Path((address, id)): Path<(String, String)>,
    State(db): State<Arc<DatabaseClient>>,
    headers: HeaderMap,
) -> Response {
    if let Some(denied) = require_mailbox_token(&db, &address, &headers).await {
        return denied;
    }
    match db
        .get_mail_by_id(
            &id,
            &address,
            &mailbox_token_from_headers(&headers).unwrap_or_default(),
        )
        .await
    {
        Ok(Some(mail)) => {
            if mail.recipients != address {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ApiResponse::<()>::error(
                        "Email not found for this address".to_string(),
                    )),
                )
                    .into_response();
            }
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::error("Email not found".to_string())),
            )
                .into_response();
        }
        Err(e) => {
            error!("Failed to get email: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response();
        }
    }

    match db
        .delete_mail(
            &id,
            &address,
            &mailbox_token_from_headers(&headers).unwrap_or_default(),
        )
        .await
    {
        Ok(_) => (StatusCode::OK, Json(ApiResponse::success(()))).into_response(),
        Err(e) => {
            error!("Failed to delete email: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::error(
                    "Internal server error".to_string(),
                )),
            )
                .into_response()
        }
    }
}

async fn admin_page() -> Html<&'static str> {
    Html(include_str!("../static/admin.html"))
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    fmt::init();

    let db = Arc::new(DatabaseClient::connect().await.unwrap());
    let access_log = Arc::new(access_log::AccessLog::new(10_000));
    let turnstile = Arc::new(turnstile::Turnstile::from_env());

    if turnstile.enabled() {
        info!("Turnstile verification is enabled for POST /api/emails");
    } else {
        warn!("Turnstile verification is disabled (TURNSTILE_ENABLED is not true)");
    }

    // Abuse alerting: a no-op when ALERT_WEBHOOK_URL is unset.
    alerts::Alerter::from_env().spawn(db.clone(), access_log.clone());

    // Start cleanup scheduler
    if let Err(e) = scheduler::start_cleanup_scheduler(db.clone()).await {
        error!("Failed to start cleanup scheduler: {}", e);
    }

    // Configure rate limiter: 200 requests per second per IP (DDoS protection)
    // Note: SmartIpKeyExtractor requires proxy headers (X-Forwarded-For, X-Real-IP)
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(200) // 200 requests per second
            .burst_size(250) // Allow burst of up to 250 requests
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
            origin_str == "https://xelio.me"
                || origin_str == "https://www.xelio.me"
                || origin_str == "http://localhost:3000"
                || origin_str.starts_with("http://localhost:")
        }))
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    let app = Router::new()
        .route("/", get(root))
        .route("/abuse", get(pages::abuse))
        .route("/.well-known/security.txt", get(pages::security_txt))
        .route("/admin", get(admin_page))
        .route("/admin-panel", get(admin_panel::page))
        .route("/admin-panel/login", post(admin_panel::login))
        .route("/admin-panel/logout", post(admin_panel::logout))
        .route("/admin-panel/export.csv", get(admin_panel::export_csv))
        .route("/admin-panel/alert-test", post(admin_panel::alert_test))
        .route("/api/emails", post(create_email))
        .route("/api/emails", get(list_emails))
        .route("/api/emails/:address", delete(delete_email_address))
        .route("/api/emails/:address", get(get_emails))
        .route("/api/emails/:address/:id", get(get_email))
        .route("/api/emails/:address/:id", delete(delete_email))
        .route("/api/stats", get(get_stats))
        .route("/api/admin/registry", get(list_registry))
        .route("/api/admin/requests", get(access_log::list_requests))
        .route(
            "/api/admin/requests/geo",
            get(access_log::list_requests_geo),
        )
        .layer(governor_layer)
        .layer(middleware::from_fn_with_state(
            access_log.clone(),
            access_log::record_request,
        ))
        .layer(middleware::from_fn(no_store))
        .layer(Extension(access_log))
        .layer(Extension(turnstile))
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

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database configured through DB_* variables"]
    async fn test_creation_conflicts_reuse_and_public_lifetime_stats() {
        async fn create(db: Arc<DatabaseClient>, username: &str) -> Response {
            create_email(
                State(db),
                Extension(Arc::new(turnstile::Turnstile::disabled())),
                HeaderMap::new(),
                Json(CreateEmailRequest::new(username)),
            )
            .await
        }

        assert!(
            env::var("DB_NAME").unwrap().ends_with("_test"),
            "use an isolated test database"
        );
        let db = Arc::new(DatabaseClient::connect().await.unwrap());
        let baseline = db.get_total_stats().await.unwrap().total_addresses_created;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let username = format!("http{suffix}");
        let response = create(db.clone(), &username).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let address = body["data"]["address"].as_str().unwrap();
        let token = body["data"]["access_token"].as_str().unwrap();
        let response = create(db.clone(), &username).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // Exercise the public route and serialized field that the client consumes.
        use tower::ServiceExt;
        let app = Router::new()
            .route("/api/stats", get(get_stats))
            .with_state(db.clone());
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/stats")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let stats: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stats["success"], true);
        assert_eq!(stats["data"]["total_addresses_created"], baseline + 1);

        db.pool.get().await.unwrap().execute(
            "UPDATE email_addresses SET created_at = ((clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours')::text WHERE address = $1", &[&address],
        ).await.unwrap();
        let response = create(db.clone(), &username).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let replacement: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let new_token = replacement["data"]["access_token"].as_str().unwrap();
        assert_ne!(token, new_token);
        let mut headers = HeaderMap::new();
        headers.insert("x-mailbox-token", token.parse().unwrap());
        let response = get_emails(Path(address.to_string()), State(db.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(db.delete_email_address(address, new_token).await.unwrap());
        assert_eq!(
            db.get_total_stats().await.unwrap().total_addresses_created,
            baseline + 2
        );
    }
}
