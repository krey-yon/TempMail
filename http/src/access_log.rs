//! In-memory access log for abuse investigation.
//!
//! The app previously logged only usernames and subjects, so there was no way
//! to attribute a burst of mailbox creations to an IP. This module records a
//! bounded ring buffer of recent requests and exposes it behind the admin
//! token, with optional IP geolocation enrichment.
//!
//! Client IP comes from the reverse proxy. Coolify's Traefik replaces
//! `X-Forwarded-For` with the peer address (it does not trust client-supplied
//! values), which is why taking the first entry is safe here. If the deployment
//! ever changes to a proxy that appends instead of replacing, inbound
//! `X-Forwarded-For` must be stripped at the edge or this becomes spoofable.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{Extension, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};

use crate::types::ApiResponse;

const DEFAULT_CAPACITY: usize = 10_000;
const GEO_MAX_IPS: usize = 500;
const GEO_BATCH_SIZE: usize = 100;
const GEO_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const GEO_FIELDS: &str =
    "status,message,country,countryCode,regionName,city,isp,org,as,mobile,proxy,hosting,query";

/// One recorded request. Only the fields an operator needs to identify abuse.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub at: String,
    pub ip: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub ms: u64,
    pub user_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Geo {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(rename = "countryCode", default)]
    pub country_code: Option<String>,
    #[serde(rename = "regionName", default)]
    pub region: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub isp: Option<String>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(rename = "as", default)]
    pub asn: Option<String>,
    #[serde(default)]
    pub mobile: bool,
    #[serde(default)]
    pub proxy: bool,
    #[serde(default)]
    pub hosting: bool,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Enriched {
    #[serde(flatten)]
    pub record: Record,
    pub geo: Option<Geo>,
}

struct CacheEntry {
    fetched: Instant,
    geo: Geo,
}

pub struct AccessLog {
    entries: RwLock<VecDeque<Record>>,
    capacity: usize,
    geo_cache: Mutex<HashMap<String, CacheEntry>>,
    http: reqwest::Client,
    geo_api: Option<String>,
    geo_lock: tokio::sync::Mutex<()>,
}

impl AccessLog {
    pub fn new(capacity: usize) -> Self {
        let geo_api = std::env::var("GEO_API_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| Some("http://ip-api.com".to_string()));
        Self {
            entries: RwLock::new(VecDeque::with_capacity(capacity.min(DEFAULT_CAPACITY))),
            capacity,
            geo_cache: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            geo_api,
            geo_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Test-only constructor: seeds rows and disables outbound geo lookups.
    #[cfg(test)]
    pub(crate) fn seeded(capacity: usize, records: Vec<Record>) -> Self {
        Self {
            entries: RwLock::new(records.into()),
            capacity,
            geo_cache: Mutex::new(HashMap::new()),
            http: reqwest::Client::new(),
            geo_api: None,
            geo_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn push(&self, record: Record) {
        let mut entries = match self.entries.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(record);
    }

    /// Newest-first snapshot, optionally limited to mailbox creations.
    pub fn snapshot(&self, limit: usize, creates_only: bool) -> Vec<Record> {
        let entries = match self.entries.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        entries
            .iter()
            .rev()
            .filter(|r| !creates_only || (r.method == "POST" && r.path == "/api/emails"))
            .take(limit)
            .cloned()
            .collect()
    }

    async fn geolocate(&self, ips: Vec<String>) -> HashMap<String, Geo> {
        let Some(api) = self.geo_api.as_deref() else {
            return HashMap::new();
        };

        let mut result = HashMap::new();
        let mut missing = Vec::new();
        {
            let cache = match self.geo_cache.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            for ip in ips {
                match cache.get(&ip) {
                    Some(entry) if entry.fetched.elapsed() < GEO_CACHE_TTL => {
                        result.insert(ip, entry.geo.clone());
                    }
                    _ => missing.push(ip),
                }
            }
        }
        if missing.is_empty() {
            return result;
        }

        // One enrichment at a time: the free geo endpoint allows 15 batches/min.
        let _guard = self.geo_lock.lock().await;

        for chunk in missing.chunks(GEO_BATCH_SIZE) {
            let url = format!("{}/batch?fields={}", api.trim_end_matches('/'), GEO_FIELDS);
            let response = self.http.post(&url).json(&chunk.to_vec()).send().await;
            let batch: Vec<Geo> = match response {
                Ok(resp) => match resp.json().await {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        tracing::warn!("Geo lookup decode failed: {}", e);
                        continue;
                    }
                },
                Err(e) => {
                    tracing::warn!("Geo lookup failed: {}", e);
                    continue;
                }
            };

            let mut cache = match self.geo_cache.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            for geo in batch {
                if geo.query.is_empty() {
                    continue;
                }
                cache.insert(
                    geo.query.clone(),
                    CacheEntry {
                        fetched: Instant::now(),
                        geo: geo.clone(),
                    },
                );
                result.insert(geo.query.clone(), geo);
            }
        }

        result
    }

    pub async fn enrich(&self, limit: usize, creates_only: bool) -> Vec<Enriched> {
        let records = self.snapshot(limit, creates_only);
        let mut unique: Vec<String> = records
            .iter()
            .map(|r| r.ip.clone())
            .filter(|ip| ip != "unknown")
            .collect();
        unique.sort();
        unique.dedup();
        unique.truncate(GEO_MAX_IPS);

        let geo = self.geolocate(unique).await;
        records
            .into_iter()
            .map(|record| Enriched {
                geo: geo.get(&record.ip).cloned(),
                record,
            })
            .collect()
    }
}

/// Extract the client IP supplied by the trusted reverse proxy.
pub fn client_ip(headers: &HeaderMap) -> String {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|part| part.parse::<std::net::IpAddr>().is_ok())
        });
    if let Some(ip) = forwarded {
        return ip.to_string();
    }
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| value.parse::<std::net::IpAddr>().is_ok())
    {
        return ip.to_string();
    }
    "unknown".to_string()
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(300).collect())
}

/// Records every request that reaches the router, including rate-limited ones.
pub async fn record_request(
    State(log): State<Arc<AccessLog>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let started = Instant::now();
    let ip = client_ip(request.headers());
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let user_agent = header_string(request.headers(), "user-agent").unwrap_or_default();
    let origin = header_string(request.headers(), "origin");
    let referer = header_string(request.headers(), "referer");

    let response = next.run(request).await;
    let status = response.status().as_u16();
    let ms = started.elapsed().as_millis() as u64;

    if method == "POST" && path == "/api/emails" {
        match status {
            201 => tracing::info!(ip = %ip, ms, ua = %user_agent, "mailbox created"),
            _ => {
                tracing::warn!(ip = %ip, ms, status, ua = %user_agent, "mailbox creation rejected")
            }
        }
    } else if status == StatusCode::TOO_MANY_REQUESTS.as_u16() {
        tracing::warn!(ip = %ip, method = %method, path = %path, "rate limited");
    }

    log.push(Record {
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ip,
        method,
        path,
        status,
        ms,
        user_agent,
        origin,
        referer,
    });

    response
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub limit: Option<usize>,
    /// Defaults to true: the abuse signal is address creation.
    pub creates_only: Option<bool>,
}

impl ListParams {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(500).clamp(1, DEFAULT_CAPACITY)
    }

    fn creates_only(&self) -> bool {
        self.creates_only.unwrap_or(true)
    }
}

pub async fn list_requests(
    Extension(log): Extension<Arc<AccessLog>>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> Response {
    if !crate::admin_authorized(&headers) {
        return unauthorized();
    }
    let data = log.snapshot(params.limit(), params.creates_only());
    (StatusCode::OK, Json(ApiResponse::success(data))).into_response()
}

pub async fn list_requests_geo(
    Extension(log): Extension<Arc<AccessLog>>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> Response {
    if !crate::admin_authorized(&headers) {
        return unauthorized();
    }
    let data = log.enrich(params.limit(), params.creates_only()).await;
    (StatusCode::OK, Json(ApiResponse::success(data))).into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiResponse::<()>::error("Admin token required".to_string())),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        map
    }

    #[test]
    fn prefers_first_forwarded_entry() {
        let h = headers(&[("x-forwarded-for", "203.0.113.9, 10.0.0.1")]);
        assert_eq!(client_ip(&h), "203.0.113.9");
    }

    #[test]
    fn falls_back_to_real_ip_then_unknown() {
        let h = headers(&[("x-real-ip", "198.51.100.4")]);
        assert_eq!(client_ip(&h), "198.51.100.4");
        assert_eq!(client_ip(&HeaderMap::new()), "unknown");
    }

    #[test]
    fn ignores_garbage_forwarded_values() {
        let h = headers(&[("x-forwarded-for", "not-an-ip, 198.51.100.7")]);
        assert_eq!(client_ip(&h), "198.51.100.7");
    }

    #[test]
    fn ring_buffer_keeps_newest_and_filters_creates() {
        let log = AccessLog::new(3);
        let record = |path: &str, method: &str| Record {
            at: "2026-10-28T00:00:00.000Z".to_string(),
            ip: "203.0.113.1".to_string(),
            method: method.to_string(),
            path: path.to_string(),
            status: 201,
            ms: 1,
            user_agent: String::new(),
            origin: None,
            referer: None,
        };
        log.push(record("/", "GET"));
        log.push(record("/api/emails", "POST"));
        log.push(record("/api/stats", "GET"));
        log.push(record("/api/emails", "POST"));

        let all = log.snapshot(10, false);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].path, "/api/emails");
        let creates = log.snapshot(10, true);
        assert_eq!(creates.len(), 2);
        assert_eq!(creates[0].method, "POST");
    }

    #[tokio::test]
    async fn admin_requests_require_token_and_return_rows() {
        std::env::set_var("ADMIN_TOKEN", "test-token");
        let log = Arc::new(AccessLog::new(8));
        log.push(Record {
            at: "2026-10-28T00:00:00.000Z".to_string(),
            ip: "203.0.113.7".to_string(),
            method: "POST".to_string(),
            path: "/api/emails".to_string(),
            status: 201,
            ms: 3,
            user_agent: "probe/1.0".to_string(),
            origin: None,
            referer: None,
        });

        let params = || {
            Query(ListParams {
                limit: Some(10),
                creates_only: Some(true),
            })
        };

        let denied = list_requests(Extension(log.clone()), HeaderMap::new(), params()).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let mut headers = HeaderMap::new();
        headers.insert("x-admin-token", "test-token".parse().unwrap());
        let allowed = list_requests(Extension(log), headers, params()).await;
        assert_eq!(allowed.status(), StatusCode::OK);

        std::env::remove_var("ADMIN_TOKEN");
    }
}
