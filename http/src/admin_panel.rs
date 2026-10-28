//! Server-rendered, password-gated admin panel.
//!
//! The page is intentionally dependency-free: no SPA, no CDN, no external
//! JavaScript or CSS. Everything an operator sees is rendered on the server
//! from the in-memory access log, so it also works with JavaScript disabled.
//!
//! Authentication is a single password from `ADMIN_PANEL_PASSWORD` and a
//! process-local session store. Sessions are lost on restart (as is the
//! access log itself), which is acceptable for an operational triage view.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    hash::{BuildHasher, Hasher},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Extension, Form, Query},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::access_log::{self, AccessLog, Enriched, Geo};

const COOKIE_NAME: &str = "xelio_panel";
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_FAILURES: u32 = 8;
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 1000;
const MAP_ZOOM: u8 = 10;

// ---------------------------------------------------------------------------
// Session and brute-force state
// ---------------------------------------------------------------------------

fn sessions() -> &'static Mutex<HashMap<String, Instant>> {
    static CELL: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn failures() -> &'static Mutex<HashMap<String, Attempts>> {
    static CELL: OnceLock<Mutex<HashMap<String, Attempts>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

struct Attempts {
    count: u32,
    started: Instant,
}

fn lock_sessions() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    match sessions().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_failures() -> std::sync::MutexGuard<'static, HashMap<String, Attempts>> {
    match failures().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn purge_sessions(sessions: &mut HashMap<String, Instant>, now: Instant) {
    sessions.retain(|_, expires_at| *expires_at > now);
}

fn create_session() -> String {
    let token = random_token();
    let now = Instant::now();
    let mut store = lock_sessions();
    purge_sessions(&mut store, now);
    store.insert(token.clone(), now + SESSION_TTL);
    token
}

fn session_valid(token: &str) -> bool {
    let now = Instant::now();
    let mut store = lock_sessions();
    purge_sessions(&mut store, now);
    store
        .get(token)
        .map(|expires_at| *expires_at > now)
        .unwrap_or(false)
}

fn destroy_session(token: &str) {
    lock_sessions().remove(token);
}

fn too_many_failures(ip: &str) -> bool {
    let now = Instant::now();
    let mut store = lock_failures();
    store.retain(|_, attempts| now.duration_since(attempts.started) < FAILURE_WINDOW);
    store
        .get(ip)
        .map(|attempts| attempts.count >= MAX_FAILURES)
        .unwrap_or(false)
}

fn record_failure(ip: &str) {
    let now = Instant::now();
    let mut store = lock_failures();
    let entry = store.entry(ip.to_string()).or_insert(Attempts {
        count: 0,
        started: now,
    });
    if now.duration_since(entry.started) >= FAILURE_WINDOW {
        entry.count = 0;
        entry.started = now;
    }
    entry.count = entry.count.saturating_add(1);
}

fn clear_failures(ip: &str) {
    lock_failures().remove(ip);
}

/// 32+ hex characters derived from `RandomState`, clock nanos, and a counter.
fn random_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut token = String::with_capacity(64);
    for round in 0..4u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u64(counter);
        hasher.write_u64(round);
        hasher.write_u32(std::process::id());
        let _ = write!(token, "{:016x}", hasher.finish());
    }
    token
}

// ---------------------------------------------------------------------------
// Password and cookie handling
// ---------------------------------------------------------------------------

fn configured_password() -> Option<String> {
    std::env::var("ADMIN_PANEL_PASSWORD")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn constant_time_eq(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    expected
        .bytes()
        .zip(presented.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let mut pieces = part.trim().splitn(2, '=');
        let name = pieces.next()?.trim();
        if name == COOKIE_NAME {
            if let Some(value) = pieces.next() {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn session_cookie(token: &str) -> String {
    format!(
        "{}={}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
        COOKIE_NAME,
        token,
        SESSION_TTL.as_secs()
    )
}

fn clearing_cookie() -> String {
    format!(
        "{}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0",
        COOKIE_NAME
    )
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct PanelParams {
    pub q: Option<String>,
    pub country: Option<String>,
    pub creates_only: Option<String>,
    pub limit: Option<String>,
    pub refresh: Option<String>,
}

impl PanelParams {
    fn limit(&self) -> usize {
        self.limit
            .as_deref()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT)
    }

    fn creates_only(&self) -> bool {
        matches!(self.creates_only.as_deref(), Some("true") | Some("1"))
    }

    fn refresh(&self) -> bool {
        matches!(self.refresh.as_deref(), Some("1") | Some("true"))
    }

    fn q(&self) -> &str {
        self.q.as_deref().unwrap_or("").trim()
    }

    fn country(&self) -> &str {
        self.country.as_deref().unwrap_or("").trim()
    }

    fn query_string(&self, with_refresh: bool) -> String {
        let mut parts = Vec::new();
        if !self.q().is_empty() {
            parts.push(format!("q={}", url_encode(self.q())));
        }
        if !self.country().is_empty() {
            parts.push(format!("country={}", url_encode(self.country())));
        }
        if self.creates_only() {
            parts.push("creates_only=true".to_string());
        }
        parts.push(format!("limit={}", self.limit()));
        if with_refresh {
            parts.push("refresh=1".to_string());
        }
        parts.join("&")
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => {
                let _ = write!(out, "%{:02X}", byte);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Escaping and small formatting helpers
// ---------------------------------------------------------------------------

fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn format_time(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| raw.chars().take(19).collect())
}

fn percent(part: usize, total: usize) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    format!("{:.0}%", (part as f64 / total as f64) * 100.0)
}

fn flag(country_code: Option<&str>) -> String {
    let Some(code) = country_code else {
        return String::new();
    };
    if code.len() != 2 || !code.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return String::new();
    }
    code.to_ascii_uppercase()
        .chars()
        .filter_map(|ch| char::from_u32(0x1F1E6 + (ch as u32 - 'A' as u32)))
        .collect()
}

fn place(geo: &Geo) -> String {
    let mut parts = Vec::new();
    if let Some(city) = geo.city.as_deref().filter(|value| !value.is_empty()) {
        parts.push(esc(city));
    }
    if let Some(region) = geo.region.as_deref().filter(|value| !value.is_empty()) {
        parts.push(esc(region));
    }
    if let Some(country) = geo
        .country
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| geo.country_code.as_deref())
    {
        parts.push(esc(country));
    }
    parts.join(", ")
}

fn location_cell(geo: Option<&Geo>, ip: &str) -> String {
    let Some(geo) = geo else {
        return "<span class=\"dim\">—</span>".to_string();
    };
    let label = place(geo);
    let flag = flag(geo.country_code.as_deref());
    let text = if label.is_empty() {
        if flag.is_empty() {
            esc(ip)
        } else {
            flag
        }
    } else if flag.is_empty() {
        label
    } else {
        format!("{} {}", flag, label)
    };

    match (geo.lat, geo.lon) {
        (Some(lat), Some(lon)) => format!(
            "<a href=\"https://www.openstreetmap.org/?mlat={lat:.4}&amp;mlon={lon:.4}#map={MAP_ZOOM}/{lat:.4}/{lon:.4}\" target=\"_blank\" rel=\"noopener\">{}</a>",
            text
        ),
        _ => text,
    }
}

fn ip_cell(geo: Option<&Geo>, ip: &str) -> String {
    let escaped = esc(ip);
    match geo.and_then(|geo| Some((geo.lat?, geo.lon?))) {
        Some((lat, lon)) => format!(
            "<a href=\"https://www.openstreetmap.org/?mlat={lat:.4}&amp;mlon={lon:.4}#map={MAP_ZOOM}/{lat:.4}/{lon:.4}\" target=\"_blank\" rel=\"noopener\">{}</a>",
            escaped
        ),
        None => format!("<b>{}</b>", escaped),
    }
}

fn network_cell(geo: Option<&Geo>) -> String {
    let Some(geo) = geo else {
        return "<span class=\"dim\">—</span>".to_string();
    };
    let mut out = String::new();
    if let Some(asn) = geo.asn.as_deref().filter(|value| !value.is_empty()) {
        let _ = write!(out, "<div>{}</div>", esc(asn));
    }
    if let Some(org) = geo
        .org
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| geo.isp.as_deref().filter(|value| !value.is_empty()))
    {
        let _ = write!(out, "<div class=\"dim\">{}</div>", esc(org));
    }
    if out.is_empty() {
        "<span class=\"dim\">—</span>".to_string()
    } else {
        out
    }
}

fn badge(label: &str, class: &str) -> String {
    format!("<span class=\"badge {}\">{}</span>", class, esc(label))
}

fn geo_badges(geo: Option<&Geo>) -> String {
    let Some(geo) = geo else {
        return String::new();
    };
    let mut out = String::new();
    if geo.hosting {
        out.push_str(&badge("hosting", "host"));
    }
    if geo.proxy {
        out.push_str(&badge("proxy", "proxy"));
    }
    if geo.mobile {
        out.push_str(&badge("mobile", "mobile"));
    }
    out
}

fn status_class(status: u16) -> &'static str {
    match status / 100 {
        1 => "s1",
        2 => "s2",
        3 => "s3",
        4 => "s4",
        5 => "s5",
        _ => "s1",
    }
}

fn chatty(value: &str, max_chars: usize) -> String {
    if value.trim().is_empty() {
        return "<span class=\"dim\">—</span>".to_string();
    }
    format!(
        "<span class=\"trunc\" title=\"{}\">{}</span>",
        esc(value),
        esc(&truncate(value, max_chars))
    )
}

// ---------------------------------------------------------------------------
// User-agent parsing (manual, no crates)
// ---------------------------------------------------------------------------

struct Device {
    os: &'static str,
    browser: &'static str,
    class: &'static str,
    automation: bool,
}

fn device(ua: &str) -> Device {
    let lower = ua.to_ascii_lowercase();
    let detected_bot = ["bot", "spider", "crawler", "headless", "wget", "scrapy"]
        .iter()
        .any(|marker| lower.contains(marker));
    let automation = detected_bot
        || [
            "python",
            "curl",
            "go-http",
            "okhttp",
            "axios",
            "node-fetch",
            "java",
            "spider",
        ]
        .iter()
        .any(|marker| lower.contains(marker));

    let os = if lower.contains("windows") {
        "Windows"
    } else if lower.contains("android") {
        "Android"
    } else if lower.contains("iphone") || lower.contains("ipad") || lower.contains("ipod") {
        "iOS"
    } else if lower.contains("mac os x") || lower.contains("macintosh") {
        "macOS"
    } else if lower.contains("linux") {
        "Linux"
    } else {
        "unknown"
    };

    let browser = if lower.contains("edg/") || lower.contains("edga/") || lower.contains("edgios/")
    {
        "Edge"
    } else if lower.contains("opr/") || lower.contains("opera") {
        "Opera"
    } else if lower.contains("python") {
        "python-requests"
    } else if lower.contains("curl/") || lower.trim() == "curl" {
        "curl"
    } else if lower.contains("go-http-client") {
        "Go-http-client"
    } else if lower.contains("okhttp") {
        "okhttp"
    } else if lower.contains("axios") {
        "axios"
    } else if lower.contains("node-fetch") {
        "node-fetch"
    } else if lower.starts_with("java/") || lower.contains("java/") {
        "Java"
    } else if lower.contains("firefox/") || lower.contains("fxios/") {
        "Firefox"
    } else if lower.contains("crios/") || lower.contains("chrome/") {
        "Chrome"
    } else if lower.contains("safari/") {
        "Safari"
    } else {
        "other"
    };

    let is_api = matches!(
        browser,
        "curl" | "python-requests" | "Go-http-client" | "okhttp" | "axios" | "node-fetch" | "Java"
    );

    let class = if detected_bot {
        "bot"
    } else if is_api {
        "api-client"
    } else if lower.contains("ipad") || lower.contains("tablet") {
        "tablet"
    } else if lower.contains("mobile") || lower.contains("iphone") || lower.contains("android") {
        "mobile"
    } else if ua.trim().is_empty() {
        "api-client"
    } else {
        "desktop"
    };

    Device {
        os,
        browser,
        class,
        automation,
    }
}

fn device_cell(ua: &str) -> String {
    if ua.trim().is_empty() {
        return "<span class=\"dim\">—</span>".to_string();
    }
    let device = device(ua);
    let mut out = format!(
        "<div>{} · {}</div><div class=\"badges\">{}</div>",
        esc(device.os),
        esc(device.browser),
        badge(device.class, "class")
    );
    if device.automation {
        out.push_str(&badge("automation", "auto"));
    }
    out
}

// ---------------------------------------------------------------------------
// Page shell
// ---------------------------------------------------------------------------

const STYLE: &str = r#"
:root {
  --bg: #0c0c0b; --panel: #131312; --panel-2: #191918; --line: #262624;
  --ink: #edeae2; --muted: #8b8880; --dim: #635f58; --accent: #d9f27e;
  --warn: #f2b06b; --bad: #f27e7e;
  --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif;
}
* { box-sizing: border-box; }
html, body { margin: 0; background: var(--bg); color: var(--ink); font-family: var(--sans); }
body { font-size: 14px; -webkit-font-smoothing: antialiased; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
code { font-family: var(--mono); color: var(--muted); }
.wrap { max-width: 1360px; margin: 0 auto; padding: 26px 22px 72px; }
header.top { display: flex; align-items: flex-start; justify-content: space-between; gap: 14px; flex-wrap: wrap; margin-bottom: 20px; }
h1 { font-size: 19px; font-weight: 600; letter-spacing: -.01em; margin: 0; }
h1 span { color: var(--dim); font-weight: 400; }
.meta { color: var(--muted); font-family: var(--mono); font-size: 11px; margin-top: 6px; }

.gate-wrap { min-height: 78vh; display: flex; align-items: center; justify-content: center; padding: 20px; }
.gate { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 26px; width: 100%; max-width: 460px; }
.gate h1 { margin-bottom: 10px; }
.gate p { color: var(--muted); line-height: 1.55; margin: 0 0 18px; }
.gate label { display: block; color: var(--dim); font-family: var(--mono); font-size: 11px; text-transform: uppercase; letter-spacing: .07em; margin-bottom: 7px; }
.gate input[type=password] { width: 100%; background: var(--panel-2); border: 1px solid var(--line); color: var(--ink); border-radius: 8px; padding: 10px 12px; font-family: var(--mono); font-size: 13px; outline: none; margin-bottom: 12px; }
.gate input[type=password]:focus { border-color: #4b4a44; }
.gate button { width: 100%; }
.err { color: var(--bad); font-family: var(--mono); font-size: 12px; margin-top: 12px; }
.note-box { background: var(--panel); border: 1px solid var(--line); border-radius: 12px; padding: 24px; max-width: 560px; margin: 12vh auto 0; }
.note-box h1 { margin-bottom: 10px; }
.note-box p { color: var(--muted); line-height: 1.6; }

button { background: var(--accent); color: #10120a; border: 0; border-radius: 8px; padding: 9px 14px; font-weight: 600; font-size: 13px; cursor: pointer; font-family: var(--sans); }
button:hover { filter: brightness(1.06); }
button.ghost { background: transparent; color: var(--muted); border: 1px solid var(--line); font-weight: 500; }
button.ghost:hover { color: var(--ink); border-color: #3c3b36; }

.cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(158px, 1fr)); gap: 10px; margin-bottom: 18px; }
.card { background: var(--panel); border: 1px solid var(--line); border-radius: 11px; padding: 13px 15px; }
.card .k { font-size: 10px; color: var(--dim); text-transform: uppercase; letter-spacing: .07em; margin-bottom: 8px; }
.card .v { font-size: 22px; font-weight: 600; font-family: var(--mono); letter-spacing: -.02em; }
.card .s { font-size: 10px; color: var(--muted); margin-top: 4px; font-family: var(--mono); }

.panel { background: var(--panel); border: 1px solid var(--line); border-radius: 11px; overflow: hidden; margin-bottom: 16px; }
.panel h2 { font-size: 11px; text-transform: uppercase; letter-spacing: .08em; color: var(--dim); margin: 0; padding: 12px 15px; border-bottom: 1px solid var(--line); font-weight: 500; }
.toolbar { display: flex; gap: 8px; padding: 11px 15px; border-bottom: 1px solid var(--line); flex-wrap: wrap; align-items: center; }
.toolbar input[type=text], .toolbar input[type=number], .toolbar select { background: var(--panel-2); border: 1px solid var(--line); color: var(--ink); border-radius: 7px; padding: 7px 9px; font-family: var(--mono); font-size: 12px; outline: none; }
.toolbar input[type=text] { width: 220px; }
.toolbar input[type=number] { width: 82px; }
.toolbar input:focus, .toolbar select:focus { border-color: #4b4a44; }
.toolbar label { color: var(--dim); font-family: var(--mono); font-size: 10px; text-transform: uppercase; letter-spacing: .06em; }
.spacer { flex: 1; }
.table-wrap { max-height: 62vh; overflow: auto; }
table { width: 100%; border-collapse: collapse; font-family: var(--mono); font-size: 11.5px; }
thead th { position: sticky; top: 0; background: var(--panel-2); z-index: 2; text-align: left; font-weight: 500; color: var(--dim); font-size: 9.5px; text-transform: uppercase; letter-spacing: .07em; padding: 8px 11px; border-bottom: 1px solid var(--line); white-space: nowrap; }
tbody td { padding: 8px 11px; border-bottom: 1px solid #1d1d1c; vertical-align: top; }
tbody tr:hover { background: #171716; }
td.nowrap { white-space: nowrap; }
.flag { margin-right: 5px; }
.muted { color: var(--muted); }
.dim { color: var(--dim); }
.trunc { display: inline-block; max-width: 300px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; vertical-align: bottom; }
.badges { margin-top: 3px; }
.badge { display: inline-block; padding: 1px 5px; border-radius: 4px; font-size: 9.5px; margin-right: 4px; letter-spacing: .02em; white-space: nowrap; }
.badge.host { background: rgba(242,176,107,.12); color: var(--warn); border: 1px solid rgba(242,176,107,.25); }
.badge.proxy { background: rgba(242,126,126,.12); color: var(--bad); border: 1px solid rgba(242,126,126,.25); }
.badge.mobile { background: rgba(217,242,126,.1); color: var(--accent); border: 1px solid rgba(217,242,126,.22); }
.badge.auto { background: rgba(242,126,126,.12); color: var(--bad); border: 1px solid rgba(242,126,126,.25); }
.badge.class { background: var(--panel-2); color: var(--muted); border: 1px solid var(--line); }
.s1, .s3 { color: var(--muted); }
.s2 { color: var(--accent); }
.s4 { color: var(--warn); }
.s5 { color: var(--bad); }
.empty { padding: 16px 15px; color: var(--dim); font-family: var(--mono); font-size: 12px; }
.filters { font-family: var(--mono); font-size: 11px; color: var(--dim); }
footer { margin-top: 18px; color: var(--dim); font-family: var(--mono); font-size: 10.5px; line-height: 1.7; }

@media (max-width: 720px) {
  .wrap { padding: 18px 12px 56px; }
  .cards { grid-template-columns: repeat(2, 1fr); }
  .toolbar input[type=text] { width: 100%; }
  .table-wrap { max-height: 56vh; }
  .trunc { max-width: 190px; }
  header.top { align-items: center; }
}
"#;

fn shell(title: &str, body: &str, refresh: Option<u64>) -> String {
    let refresh_meta = refresh
        .map(|seconds| format!("<meta http-equiv=\"refresh\" content=\"{}\">", seconds))
        .unwrap_or_default();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"robots\" content=\"noindex, nofollow\">\n\
{refresh_meta}<title>{title}</title>\n<style>{style}</style>\n</head>\n\
<body>{body}</body>\n</html>",
        refresh_meta = refresh_meta,
        title = esc(title),
        style = STYLE,
        body = body
    )
}

fn html_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Page renderers
// ---------------------------------------------------------------------------

fn render_disabled() -> Response {
    let body = format!(
        "<main class=\"note-box\"><h1>Admin panel disabled</h1>\
<p>The panel is gated by <code>ADMIN_PANEL_PASSWORD</code>, which is not set on this server.</p>\
<p>Set the environment variable and restart the HTTP service. The panel fails closed and \
never uses a default password.</p></main>"
    );
    html_response(
        StatusCode::SERVICE_UNAVAILABLE,
        shell("Admin panel disabled", &body, None),
    )
}

fn render_too_many() -> Response {
    let body = "<main class=\"note-box\"><h1>Too many attempts</h1>\
<p>This IP has failed to sign in 8 or more times in the last 10 minutes.</p>\
<p>Wait for the window to expire, then try again.</p>\
<p><a href=\"/admin-panel\">Back to sign in</a></p></main>";
    html_response(
        StatusCode::TOO_MANY_REQUESTS,
        shell("Too many attempts", body, None),
    )
}

fn render_login(error: Option<&str>, status: StatusCode) -> Response {
    let error = error
        .map(|message| format!("<p class=\"err\">{}</p>", esc(message)))
        .unwrap_or_default();
    let body = format!(
        "<main class=\"gate-wrap\"><section class=\"gate\">\
<h1>Xelio <span>· admin</span></h1>\
<p>Recent request activity, IP attribution, and abuse signals. \
Enter the panel password to continue.</p>\
<form method=\"post\" action=\"/admin-panel/login\">\
<label for=\"password\">Password</label>\
<input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" autofocus required>\
<button type=\"submit\">Unlock</button>\
</form>{error}</section></main>"
    );
    html_response(status, shell("Xelio · admin", &body, None))
}

fn card(label: &str, value: String, sub: String) -> String {
    format!(
        "<div class=\"card\"><div class=\"k\">{}</div><div class=\"v\">{}</div><div class=\"s\">{}</div></div>",
        esc(label),
        esc(&value),
        esc(&sub)
    )
}

fn matches_filters(row: &Enriched, q: &str, country: &str) -> bool {
    if !country.is_empty() {
        let code = row
            .geo
            .as_ref()
            .and_then(|geo| geo.country_code.as_deref())
            .unwrap_or("");
        if !code.eq_ignore_ascii_case(country) {
            return false;
        }
    }
    if q.is_empty() {
        return true;
    }
    let needle = q.to_lowercase();
    let geo = row.geo.as_ref();
    let haystack = [
        row.record.ip.as_str(),
        geo.and_then(|geo| geo.country_code.as_deref())
            .unwrap_or(""),
        geo.and_then(|geo| geo.country.as_deref()).unwrap_or(""),
        geo.and_then(|geo| geo.city.as_deref()).unwrap_or(""),
        geo.and_then(|geo| geo.asn.as_deref()).unwrap_or(""),
        geo.and_then(|geo| geo.org.as_deref()).unwrap_or(""),
        geo.and_then(|geo| geo.isp.as_deref()).unwrap_or(""),
        row.record.user_agent.as_str(),
    ];
    haystack
        .iter()
        .any(|value| value.to_lowercase().contains(&needle))
}

#[derive(Default)]
struct IpStat {
    count: usize,
    first: Option<String>,
    last: Option<String>,
    last_path: String,
    geo: Option<Geo>,
    user_agents: HashSet<String>,
}

fn aggregate_ips(rows: &[&Enriched]) -> Vec<(String, IpStat)> {
    let mut stats: HashMap<String, IpStat> = HashMap::new();
    for row in rows {
        let entry = stats.entry(row.record.ip.clone()).or_default();
        entry.count += 1;
        if entry
            .first
            .as_deref()
            .map(|first| row.record.at.as_str() < first)
            .unwrap_or(true)
        {
            entry.first = Some(row.record.at.clone());
        }
        if entry
            .last
            .as_deref()
            .map(|last| row.record.at.as_str() > last)
            .unwrap_or(true)
        {
            entry.last = Some(row.record.at.clone());
        }
        if entry.last_path.is_empty() {
            entry.last_path = row.record.path.clone();
        }
        if entry.geo.is_none() && row.geo.is_some() {
            entry.geo = row.geo.clone();
        }
        entry.user_agents.insert(row.record.user_agent.clone());
    }
    let mut aggregated: Vec<(String, IpStat)> = stats.into_iter().collect();
    aggregated.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
    aggregated.truncate(50);
    aggregated
}

fn render_dashboard(rows: &[Enriched], params: &PanelParams) -> Response {
    let q = params.q();
    let country = params.country();
    let filtered: Vec<&Enriched> = rows
        .iter()
        .filter(|row| matches_filters(row, q, country))
        .collect();

    let total = filtered.len();
    let unique_ips: HashSet<&str> = filtered.iter().map(|row| row.record.ip.as_str()).collect();
    let countries: HashSet<&str> = filtered
        .iter()
        .filter_map(|row| row.geo.as_ref().and_then(|geo| geo.country_code.as_deref()))
        .filter(|code| !code.is_empty())
        .collect();
    let geo_rows = filtered.iter().filter(|row| row.geo.is_some()).count();
    let hosting = filtered
        .iter()
        .filter(|row| row.geo.as_ref().map(|geo| geo.hosting).unwrap_or(false))
        .count();
    let mobile = filtered
        .iter()
        .filter(|row| row.geo.as_ref().map(|geo| geo.mobile).unwrap_or(false))
        .count();
    let non_browser = filtered
        .iter()
        .filter(|row| row.record.origin.is_none())
        .count();

    let cards = format!(
        "{}{}{}{}{}{}",
        card(
            "Total rows",
            total.to_string(),
            format!("limit {}", params.limit())
        ),
        card(
            "Unique IPs",
            unique_ips.len().to_string(),
            "in view".to_string()
        ),
        card(
            "Countries",
            countries.len().to_string(),
            "in view".to_string()
        ),
        card(
            "Datacenter / hosting",
            percent(hosting, geo_rows),
            format!("{} of {} geolocated", hosting, geo_rows)
        ),
        card(
            "Mobile networks",
            percent(mobile, geo_rows),
            format!("{} of {} geolocated", mobile, geo_rows)
        ),
        card(
            "Non-browser",
            percent(non_browser, total),
            format!("no Origin header · {}", non_browser)
        ),
    );

    let aggregated = aggregate_ips(&filtered);

    let mut ip_rows = String::new();
    for (ip, stat) in &aggregated {
        let geo = stat.geo.as_ref();
        let _ = write!(
            ip_rows,
            "<tr>\
<td class=\"ip nowrap\">{}</td>\
<td>{}</td>\
<td class=\"nowrap\">{}</td>\
<td class=\"nowrap\">{}</td>\
<td>{}</td>\
<td>{}{}</td>\
<td>{}</td>\
<td class=\"nowrap\">{}</td>\
</tr>",
            ip_cell(geo, ip),
            stat.count,
            esc(&stat.first.as_deref().map(format_time).unwrap_or_default()),
            esc(&stat.last.as_deref().map(format_time).unwrap_or_default()),
            location_cell(geo, ip),
            network_cell(geo),
            geo_badges(geo),
            stat.user_agents.len(),
            esc(&stat.last_path),
        );
    }
    if ip_rows.is_empty() {
        ip_rows =
            "<tr><td colspan=\"9\" class=\"empty\">No matching requests in the buffer.</td></tr>"
                .to_string();
    }

    let mut request_rows = String::new();
    for row in &filtered {
        let geo = row.geo.as_ref();
        let status = row.record.status;
        let mut origin_cell = String::new();
        if let Some(origin) = row
            .record
            .origin
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let _ = write!(
                origin_cell,
                "<div><span class=\"dim\">O</span> {}</div>",
                chatty(origin, 48)
            );
        }
        if let Some(referer) = row
            .record
            .referer
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let _ = write!(
                origin_cell,
                "<div><span class=\"dim\">R</span> {}</div>",
                chatty(referer, 48)
            );
        }
        if origin_cell.is_empty() {
            origin_cell = "<span class=\"dim\">—</span>".to_string();
        }

        let _ = write!(
            request_rows,
            "<tr>\
<td class=\"nowrap\">{}</td>\
<td class=\"ip nowrap\">{}</td>\
<td>{}</td>\
<td>{}</td>\
<td>{}</td>\
<td class=\"nowrap\">{} {}</td>\
<td class=\"nowrap {}\">{}</td>\
<td class=\"nowrap\">{}</td>\
<td>{}</td>\
<td>{}</td>\
</tr>",
            esc(&format_time(&row.record.at)),
            ip_cell(geo, &row.record.ip),
            location_cell(geo, &row.record.ip),
            network_cell(geo),
            device_cell(&row.record.user_agent),
            esc(&row.record.method),
            esc(&row.record.path),
            status_class(status),
            status,
            row.record.ms,
            chatty(&row.record.user_agent, 64),
            origin_cell,
        );
    }
    if request_rows.is_empty() {
        request_rows =
            "<tr><td colspan=\"10\" class=\"empty\">No matching requests in the buffer.</td></tr>"
                .to_string();
    }

    let refresh_href = format!("/admin-panel?{}", params.query_string(params.refresh()));
    let auto_href = if params.refresh() {
        format!("/admin-panel?{}", params.query_string(false))
    } else {
        format!("/admin-panel?{}&refresh=1", params.query_string(false))
    };
    let auto_label = if params.refresh() {
        "Stop auto-refresh"
    } else {
        "Auto-refresh 30s"
    };

    let country_selected = country.to_ascii_uppercase();
    let true_selected = if params.creates_only() {
        " selected"
    } else {
        ""
    };
    let false_selected = if params.creates_only() {
        ""
    } else {
        " selected"
    };

    let body = format!(
        "<div class=\"wrap\">\
<header class=\"top\">\
<div><h1>Xelio <span>· admin panel</span></h1>\
<div class=\"meta\">Request activity · in-memory · times UTC</div></div>\
<div class=\"filters\">\
<a href=\"{refresh_href}\">Refresh</a> · <a href=\"{auto_href}\">{auto_label}</a>\
<form method=\"post\" action=\"/admin-panel/logout\" style=\"display:inline;margin-left:10px\">\
<button class=\"ghost\" type=\"submit\">Log out</button></form>\
</div></header>\
<section class=\"cards\">{cards}</section>\
<section class=\"panel\">\
<h2>Top IPs</h2>\
<div class=\"table-wrap\"><table>\
<thead><tr><th>IP</th><th>Requests</th><th>First seen</th><th>Last seen</th><th>Country</th>\
<th>ASN / network</th><th>Signals</th><th>UAs</th><th>Last path</th></tr></thead>\
<tbody>{ip_rows}</tbody></table></div></section>\
<section class=\"panel\">\
<h2>Recent requests</h2>\
<form class=\"toolbar\" method=\"get\" action=\"/admin-panel\">\
<label for=\"q\">Filter</label>\
<input id=\"q\" name=\"q\" type=\"text\" value=\"{q_value}\" placeholder=\"ip, country, city, asn, org, ua\" spellcheck=\"false\">\
<label for=\"country\">Country</label>\
<input id=\"country\" name=\"country\" type=\"text\" value=\"{country_value}\" placeholder=\"US\" maxlength=\"2\" spellcheck=\"false\">\
<label for=\"creates_only\">Creates only</label>\
<select id=\"creates_only\" name=\"creates_only\">\
<option value=\"false\"{false_selected}>false</option>\
<option value=\"true\"{true_selected}>true</option></select>\
<label for=\"limit\">Limit</label>\
<input id=\"limit\" name=\"limit\" type=\"number\" min=\"1\" max=\"1000\" value=\"{limit_value}\">\
<input type=\"hidden\" name=\"refresh\" value=\"{refresh_value}\">\
<button class=\"ghost\" type=\"submit\">Apply</button>\
<span class=\"spacer\"></span>\
<span class=\"dim\">rows {total} · creates {creates_selected_note}</span>\
</form>\
<div class=\"table-wrap\"><table>\
<thead><tr><th>Time (UTC)</th><th>IP</th><th>Location</th><th>Network</th><th>Device</th>\
<th>Request</th><th>Status</th><th>ms</th><th>User agent</th><th>Origin / Referer</th></tr></thead>\
<tbody>{request_rows}</tbody></table></div></section>\
<footer>In-memory buffer (newest 10,000 requests) · resets on deploy · geo cached 24h · times UTC.\
<br>Non-browser means the request carried no <code>Origin</code> header. Automation detection is a \
heuristic over the user agent.</footer>\
</div>",
        refresh_href = esc(&refresh_href),
        auto_href = esc(&auto_href),
        auto_label = auto_label,
        cards = cards,
        ip_rows = ip_rows,
        q_value = esc(q),
        country_value = esc(&country_selected),
        true_selected = true_selected,
        false_selected = false_selected,
        limit_value = params.limit(),
        refresh_value = if params.refresh() { "1" } else { "0" },
        total = total,
        creates_selected_note = if params.creates_only() { "on" } else { "off" },
        request_rows = request_rows,
    );

    html_response(
        StatusCode::OK,
        shell("Xelio · admin panel", &body, params.refresh().then_some(30)),
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub password: String,
}

pub async fn page(
    Extension(log): Extension<Arc<AccessLog>>,
    headers: HeaderMap,
    Query(params): Query<PanelParams>,
) -> Response {
    if configured_password().is_none() {
        return render_disabled();
    }

    match cookie_token(&headers) {
        Some(token) if session_valid(&token) => {
            let rows = log.enrich(params.limit(), params.creates_only()).await;
            render_dashboard(&rows, &params)
        }
        _ => render_login(None, StatusCode::OK),
    }
}

pub async fn login(headers: HeaderMap, Form(form): Form<LoginForm>) -> Response {
    let Some(expected) = configured_password() else {
        return render_disabled();
    };

    let ip = access_log::client_ip(&headers);
    if too_many_failures(&ip) {
        tracing::warn!(ip = %ip, "admin panel login blocked");
        return render_too_many();
    }

    if !constant_time_eq(&expected, &form.password) {
        record_failure(&ip);
        tracing::warn!(ip = %ip, "admin panel login failed");
        if too_many_failures(&ip) {
            return render_too_many();
        }
        return render_login(Some("Incorrect password."), StatusCode::UNAUTHORIZED);
    }

    clear_failures(&ip);
    let token = create_session();
    tracing::info!(ip = %ip, "admin panel login succeeded");

    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/admin-panel"));
    match HeaderValue::from_str(&session_cookie(&token)) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
            response
        }
        Err(error) => {
            tracing::error!("Failed to build admin session cookie: {}", error);
            response
        }
    }
}

pub async fn logout(headers: HeaderMap) -> Response {
    if configured_password().is_none() {
        return render_disabled();
    }

    if let Some(token) = cookie_token(&headers) {
        destroy_session(&token);
    }
    tracing::info!(ip = %access_log::client_ip(&headers), "admin panel logout");

    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("/admin-panel"));
    match HeaderValue::from_str(&clearing_cookie()) {
        Ok(value) => {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        Err(error) => tracing::error!("Failed to build clearing cookie: {}", error),
    }
    response
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_log::Record;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn reset_state() {
        lock_sessions().clear();
        lock_failures().clear();
    }

    fn seeded_log(ip: &str, ua: &str) -> Arc<AccessLog> {
        Arc::new(AccessLog::seeded(
            16,
            vec![Record {
                at: "2026-10-28T00:00:00.000Z".to_string(),
                ip: ip.to_string(),
                method: "POST".to_string(),
                path: "/api/emails".to_string(),
                status: 201,
                ms: 7,
                user_agent: ua.to_string(),
                origin: None,
                referer: None,
            }],
        ))
    }

    fn app(log: Arc<AccessLog>) -> Router {
        Router::new()
            .route("/admin-panel", get(page))
            .route("/admin-panel/login", post(login))
            .route("/admin-panel/logout", post(logout))
            .layer(Extension(log))
    }

    fn request(method: &str, uri: &str, body: Body, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(body).expect("request")
    }

    async fn body_string(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn disabled_without_password() {
        let _guard = lock_env();
        reset_state();
        std::env::remove_var("ADMIN_PANEL_PASSWORD");

        let response = app(seeded_log("203.0.113.42", "probe"))
            .oneshot(request("GET", "/admin-panel", Body::empty(), &[]))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response).await;
        assert!(body.contains("ADMIN_PANEL_PASSWORD"));
    }

    #[tokio::test]
    async fn wrong_password_is_rejected_without_cookie() {
        let _guard = lock_env();
        reset_state();
        std::env::set_var("ADMIN_PANEL_PASSWORD", "correct-horse");

        let response = app(seeded_log("203.0.113.42", "probe"))
            .oneshot(request(
                "POST",
                "/admin-panel/login",
                Body::from("password=wrong"),
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("x-forwarded-for", "198.51.100.11"),
                ],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(header::SET_COOKIE).is_none());

        std::env::remove_var("ADMIN_PANEL_PASSWORD");
    }

    #[tokio::test]
    async fn correct_password_sets_hardened_cookie() {
        let _guard = lock_env();
        reset_state();
        std::env::set_var("ADMIN_PANEL_PASSWORD", "correct-horse");

        let response = app(seeded_log("203.0.113.42", "probe"))
            .oneshot(request(
                "POST",
                "/admin-panel/login",
                Body::from("password=correct-horse"),
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("x-forwarded-for", "198.51.100.12"),
                ],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("set-cookie")
            .to_str()
            .expect("cookie");
        assert!(cookie.contains("xelio_panel="));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Max-Age=43200"));

        std::env::remove_var("ADMIN_PANEL_PASSWORD");
    }

    #[tokio::test]
    async fn dashboard_with_session_shows_seeded_ip() {
        let _guard = lock_env();
        reset_state();
        std::env::set_var("ADMIN_PANEL_PASSWORD", "correct-horse");

        let log = seeded_log("203.0.113.42", "curl/8.4.0");
        let token = create_session();
        let response = app(log)
            .oneshot(request(
                "GET",
                "/admin-panel",
                Body::empty(),
                &[("cookie", &format!("xelio_panel={}", token))],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response).await;
        assert!(body.contains("203.0.113.42"));
        assert!(body.contains("Recent requests"));
        assert!(body.contains("Log out"));

        std::env::remove_var("ADMIN_PANEL_PASSWORD");
    }

    #[tokio::test]
    async fn dashboard_without_cookie_renders_login() {
        let _guard = lock_env();
        reset_state();
        std::env::set_var("ADMIN_PANEL_PASSWORD", "correct-horse");

        let response = app(seeded_log("203.0.113.42", "probe"))
            .oneshot(request("GET", "/admin-panel", Body::empty(), &[]))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response).await;
        assert!(body.contains("type=\"password\""));
        assert!(body.contains("/admin-panel/login"));

        std::env::remove_var("ADMIN_PANEL_PASSWORD");
    }

    #[tokio::test]
    async fn expired_session_is_rejected() {
        let _guard = lock_env();
        reset_state();
        std::env::set_var("ADMIN_PANEL_PASSWORD", "correct-horse");

        let expired = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("instant");
        lock_sessions().insert("expired-token".to_string(), expired);

        let response = app(seeded_log("203.0.113.42", "probe"))
            .oneshot(request(
                "GET",
                "/admin-panel",
                Body::empty(),
                &[("cookie", "xelio_panel=expired-token")],
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response).await;
        assert!(body.contains("type=\"password\""));
        assert!(!body.contains("203.0.113.42"));

        std::env::remove_var("ADMIN_PANEL_PASSWORD");
    }
}
