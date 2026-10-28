use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CreateEmailRequest {
    pub username: String,
    /// Cloudflare Turnstile widget token. Optional in the type so the request
    /// still deserializes when the gate is off; the handler rejects a missing
    /// token when `TURNSTILE_ENABLED` is true.
    #[serde(default)]
    pub turnstile_token: Option<String>,
}

impl CreateEmailRequest {
    /// Convenience for callers and tests that do not carry a token.
    #[cfg(test)]
    pub fn new(username: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            turnstile_token: None,
        }
    }
}

#[derive(Serialize)]
pub struct DeleteEmailResponse {
    pub message: String,
    pub address: String,
}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
    /// Stable machine-readable reason, present only on some errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            code: None,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
            code: None,
        }
    }

    pub fn error_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
            code: Some(code.into()),
        }
    }
}
