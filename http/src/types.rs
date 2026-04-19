use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct CreateEmailRequest {
    pub username: String,
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
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
        }
    }
}
