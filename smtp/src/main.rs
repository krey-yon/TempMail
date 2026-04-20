use dotenv::dotenv;
use smtp::start_smtp_server;
use tracing::error;
use std::env;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt::init();

    if dotenv().is_err(){
        error!("Failed to load .env file");
    }

    let port: u16 = env::var("SMTP_PORT")
        .unwrap_or_else(|_| "25".to_string())
        .parse()
        .expect("SMTP_PORT must be a valid port number");

    let addr: std::net::SocketAddr = format!("0.0.0.0:{}", port)
        .parse()
        .unwrap();

    let domain = env::var("MAIL_DOMAIN")
        .unwrap_or_else(|_| "mail.kreyon.in".to_string());

    start_smtp_server(addr, domain).await
}
