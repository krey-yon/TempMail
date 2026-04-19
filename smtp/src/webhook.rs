use regex::Regex;
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct Payload {
    pub version: i32,
    pub otp: String,
    pub mail: String,
}

pub async fn send_webhook(webhook_url: &str, payload: &Payload) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let response = client
        .post(webhook_url)
        .json(payload)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Webhook request failed with status: {}", response.status()).into());
    }

    Ok(())
}

pub fn extract_otp(content: &str) -> String {
    let patterns = [
        r"(?i)otp[:\s]*(\d{6})",
        r"(?i)verification[:\s]*(\d{6})",
        r"(?i)code[:\s]*(\d{6})",
        r"(?i)passcode[:\s]*(\d{6})",
        r"\b(\d{6})\b",
    ];

    for pattern in &patterns {
        if let Ok(re) = Regex::new(pattern) {
            if let Some(caps) = re.captures(content) {
                if let Some(otp) = caps.get(1) {
                    return otp.as_str().to_string();
                }
            }
        }
    }

    String::new()
}
