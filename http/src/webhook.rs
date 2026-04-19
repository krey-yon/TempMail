#![allow(dead_code)]

use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct Payload {
    pub version: i32,
    pub otp: String,
    pub mail: String,
}

#[derive(Debug, Deserialize)]
struct WebhookResponse {}

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
    // Try to find 6-digit OTP codes
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_otp_six_digits() {
        assert_eq!(extract_otp("Your OTP is 123456"), "123456");
        assert_eq!(extract_otp("OTP: 654321"), "654321");
        assert_eq!(extract_otp("Verification code: 111222"), "111222");
        assert_eq!(extract_otp("Your code is 999888"), "999888");
    }

    #[test]
    fn test_extract_otp_case_insensitive() {
        assert_eq!(extract_otp("otp is 123456"), "123456");
        assert_eq!(extract_otp("OTP: 654321"), "654321");
        assert_eq!(extract_otp("Verification Code: 111222"), "111222");
    }

    #[test]
    fn test_extract_otp_no_otp() {
        assert_eq!(extract_otp("No OTP here"), "");
        assert_eq!(extract_otp("Just some text"), "");
        assert_eq!(extract_otp("12345 is too short"), "");
    }

    #[test]
    fn test_extract_otp_first_match() {
        // Should return the first 6-digit number found
        assert_eq!(extract_otp("First 123456 then 789012"), "123456");
    }
}
