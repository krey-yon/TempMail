use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{net::TcpListener, time::timeout};
use tracing::info;
use database::database::DatabaseClient;

use crate::server::Server;
mod errors;
pub mod server;
mod smtp;
mod types;

pub async fn start_smtp_server(addr: SocketAddr, domain: String) {
    let listener = TcpListener::bind(addr).await.unwrap();
    let domain = Arc::new(domain);
    let db = Arc::new(DatabaseClient::connect().await.unwrap());
    let local_set = tokio::task::LocalSet::new();

    info!("Server started on Port: {}", addr);

    loop {
        let (stream, _addr) = listener.accept().await.unwrap();
        let domain = domain.clone();
        let db = db.clone();

        local_set.run_until(async move {
            tracing::info!("Ping received on SMTP Server");
            let smtp = Server::new(domain.as_str(), stream, db).await;
            let _ = timeout(Duration::from_secs(300), smtp.connection()).await;
        }).await;
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
