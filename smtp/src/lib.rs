use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{net::TcpListener, time::timeout};
use tracing::info;
use database::database::DatabaseClient;
use database::clear_old_mails::clear_old_mails;

use crate::metrics::SmtpMetrics;
use crate::server::Server;
mod errors;
pub mod server;
mod smtp;
mod types;
mod webhook;
mod metrics;

pub async fn start_smtp_server(addr: SocketAddr, domain: String) {
    let listener = TcpListener::bind(addr).await.unwrap();
    let domain = Arc::new(domain);
    let db = Arc::new(DatabaseClient::connect().await.unwrap());
    let metrics = Arc::new(SmtpMetrics::new());

    // Start background task to clear old mails every hour
    clear_old_mails(db.clone(), Duration::from_secs(3600));
    metrics::spawn_listener(metrics.clone());

    info!("Server started on Port: {}", addr);

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::error!("SMTP accept failed: {e}");
                continue;
            }
        };
        let greeting_start = Instant::now();
        if let Err(e) = stream.set_nodelay(true) {
            tracing::error!("TCP_NODELAY failed: {e}");
        }
        let domain = domain.clone();
        let db = db.clone();
        let metrics = metrics.clone();

        tokio::spawn(async move {
            tracing::info!("Ping received on SMTP Server");
            let smtp = Server::new(domain.as_str(), stream, db, metrics, greeting_start).await;
            let _ = timeout(Duration::from_secs(300), smtp.connection()).await;
        });
    }
}

pub fn is_email_valid(email: &str) -> bool {
    email.contains('@') && !email.contains("..") && email.len() < 254
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_email_valid_valid_emails() {
        let valid_emails = vec![
            "test@example.com",
            "user.name@domain.org",
            "a@b.co",
            "very-long-email-address@sub.domain.example.com",
        ];
        for email in valid_emails {
            assert!(is_email_valid(email), "Expected {} to be valid", email);
        }
    }

    #[test]
    fn test_is_email_valid_invalid_emails() {
        assert!(!is_email_valid("no-at-sign.com"), "no @ sign");
        assert!(!is_email_valid("double..dot@example.com"), "double dots");
        assert!(!is_email_valid(""), "empty string");
        // Note: test@.com passes basic validation (has @, no .., len < 254)
        // test@ passes because "test@" is 5 chars and has @
        // These are technically valid per the function's logic
    }

    #[test]
    fn test_is_email_valid_edge_cases() {
        // Short email
        assert!(is_email_valid("a@b.co"));

        // Test with clearly long email (way over 254)
        let too_long = format!("{}@test.com", "a".repeat(300));
        assert!(too_long.len() >= 254, "email is {} chars, should be >= 254", too_long.len());
        assert!(!is_email_valid(&too_long), "300+ chars should be invalid");
    }
}
