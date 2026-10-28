//! Cloudflare Turnstile verification for mailbox creation.
//!
//! Turnstile is the only gate that survives a residential proxy pool: the
//! attacker in front of this service rotates IPs across consumer ISPs and
//! spoofs a desktop Chrome user agent, so IP and UA checks cannot separate
//! them from real users. A token minted by the widget can.
//!
//! Verification is optional at build time and controlled by
//! `TURNSTILE_ENABLED`. When disabled the API behaves exactly as before, which
//! keeps the rollout (and the rollback) a single environment change.

use std::env;
use std::time::Duration;

use serde::Deserialize;

const DEFAULT_VERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const VERIFY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnstileError {
    /// The request carried no token.
    Missing,
    /// Cloudflare rejected the token, or it was minted for another site/action.
    Rejected(Vec<String>),
    /// The verifier could not be reached, or `TURNSTILE_ENABLED` is misconfigured.
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct SiteverifyResponse {
    success: bool,
    #[serde(rename = "error-codes", default)]
    error_codes: Vec<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

pub struct Turnstile {
    enabled: bool,
    secret: String,
    verify_url: String,
    hostnames: Vec<String>,
    action: Option<String>,
    client: reqwest::Client,
}

impl Turnstile {
    pub fn from_env() -> Self {
        let enabled = env_bool("TURNSTILE_ENABLED", false);
        let secret = env::var("TURNSTILE_SECRET_KEY").unwrap_or_default();
        if enabled && secret.trim().is_empty() {
            tracing::error!(
                "TURNSTILE_ENABLED is true but TURNSTILE_SECRET_KEY is empty; \
                 every mailbox creation will be rejected"
            );
        }
        Self {
            enabled,
            secret,
            verify_url: env::var("TURNSTILE_VERIFY_URL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_VERIFY_URL.to_string()),
            hostnames: env_list("TURNSTILE_REQUIRED_HOSTNAMES"),
            action: env::var("TURNSTILE_EXPECTED_ACTION")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            client: reqwest::Client::builder()
                .timeout(VERIFY_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Verify a widget token against the siteverify endpoint.
    ///
    /// Checks `success`, then the hostname allowlist, then the widget action.
    /// A network failure is reported as `Unavailable`; the caller decides
    /// whether to fail closed.
    pub async fn verify(&self, token: Option<&str>, client_ip: &str) -> Result<(), TurnstileError> {
        if !self.enabled {
            return Ok(());
        }
        let Some(token) = token.map(str::trim).filter(|value| !value.is_empty()) else {
            return Err(TurnstileError::Missing);
        };
        if self.secret.is_empty() {
            return Err(TurnstileError::Unavailable);
        }

        let body = format!(
            "secret={}&response={}&remoteip={}",
            percent_encode(&self.secret),
            percent_encode(token),
            percent_encode(client_ip)
        );
        let response = self
            .client
            .post(&self.verify_url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!("Turnstile verification request failed: {}", error);
                TurnstileError::Unavailable
            })?;

        if !response.status().is_success() {
            tracing::warn!("Turnstile verification returned HTTP {}", response.status());
            return Err(TurnstileError::Unavailable);
        }

        let body: SiteverifyResponse = response.json().await.map_err(|error| {
            tracing::warn!("Turnstile verification response was not JSON: {}", error);
            TurnstileError::Unavailable
        })?;

        if !body.success {
            return Err(TurnstileError::Rejected(body.error_codes));
        }

        if !self.hostnames.is_empty() {
            let hostname = body.hostname.as_deref().unwrap_or_default();
            if !self
                .hostnames
                .iter()
                .any(|expected| expected.eq_ignore_ascii_case(hostname))
            {
                return Err(TurnstileError::Rejected(vec![format!(
                    "hostname-mismatch:{hostname}"
                )]));
            }
        }

        if let Some(expected) = self.action.as_deref() {
            let action = body.action.as_deref().unwrap_or_default();
            if !expected.eq_ignore_ascii_case(action) {
                return Err(TurnstileError::Rejected(vec![format!(
                    "action-mismatch:{action}"
                )]));
            }
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            enabled: false,
            secret: String::new(),
            verify_url: "http://127.0.0.1:1/siteverify".to_string(),
            hostnames: Vec::new(),
            action: None,
            client: reqwest::Client::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        verify_url: impl Into<String>,
        hostnames: &[&str],
        action: Option<&str>,
    ) -> Self {
        Self {
            enabled: true,
            secret: "test-secret".to_string(),
            verify_url: verify_url.into(),
            hostnames: hostnames.iter().map(|value| value.to_string()).collect(),
            action: action.map(|value| value.to_string()),
            client: reqwest::Client::builder()
                .timeout(VERIFY_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

fn env_list(name: &str) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| part.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::json;

    async fn mock_verifier(body: serde_json::Value) -> String {
        let handler_body = body.clone();
        let app = Router::new().route(
            "/siteverify",
            post(move || {
                let body = handler_body.clone();
                async move { Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/siteverify")
    }

    #[tokio::test]
    async fn disabled_gate_accepts_anything() {
        let gate = Turnstile {
            enabled: false,
            secret: String::new(),
            verify_url: "http://127.0.0.1:1/siteverify".to_string(),
            hostnames: Vec::new(),
            action: None,
            client: reqwest::Client::new(),
        };
        assert!(gate.verify(None, "203.0.113.1").await.is_ok());
    }

    #[tokio::test]
    async fn missing_token_is_rejected_without_a_call() {
        let gate = Turnstile::for_test("http://127.0.0.1:1/siteverify", &[], None);
        assert_eq!(
            gate.verify(None, "203.0.113.1").await,
            Err(TurnstileError::Missing)
        );
        assert_eq!(
            gate.verify(Some("   "), "203.0.113.1").await,
            Err(TurnstileError::Missing)
        );
    }

    #[tokio::test]
    async fn successful_verification_passes() {
        let url = mock_verifier(json!({
            "success": true,
            "hostname": "www.xelio.me",
            "action": "create"
        }))
        .await;
        let gate = Turnstile::for_test(url, &["xelio.me", "www.xelio.me"], Some("create"));
        assert!(gate.verify(Some("token"), "203.0.113.1").await.is_ok());
    }

    #[tokio::test]
    async fn cloudflare_failure_codes_are_reported() {
        let url = mock_verifier(json!({
            "success": false,
            "error-codes": ["invalid-input-response"]
        }))
        .await;
        let gate = Turnstile::for_test(url, &[], None);
        assert_eq!(
            gate.verify(Some("token"), "203.0.113.1").await,
            Err(TurnstileError::Rejected(vec![
                "invalid-input-response".to_string()
            ]))
        );
    }

    #[tokio::test]
    async fn wrong_hostname_is_rejected() {
        let url = mock_verifier(json!({
            "success": true,
            "hostname": "attacker.example"
        }))
        .await;
        let gate = Turnstile::for_test(url, &["xelio.me"], None);
        assert!(matches!(
            gate.verify(Some("token"), "203.0.113.1").await,
            Err(TurnstileError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn wrong_action_is_rejected() {
        let url = mock_verifier(json!({
            "success": true,
            "hostname": "xelio.me",
            "action": "login"
        }))
        .await;
        let gate = Turnstile::for_test(url, &["xelio.me"], Some("create"));
        assert!(matches!(
            gate.verify(Some("token"), "203.0.113.1").await,
            Err(TurnstileError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn unreachable_verifier_is_unavailable() {
        let gate = Turnstile::for_test("http://127.0.0.1:1/siteverify", &[], None);
        assert_eq!(
            gate.verify(Some("token"), "203.0.113.1").await,
            Err(TurnstileError::Unavailable)
        );
    }
}
