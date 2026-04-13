use dotenv::dotenv;
use smtp::start_smtp_server;
use tracing::error;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt::init();

    if dotenv().is_err(){
        error!("Failed to load .env file");
    }

    let addr:std::net::SocketAddr = "0.0.0.0:25".parse().unwrap();
    let domain = String::from("mail.kreyon.in");

    start_smtp_server(addr, domain).await
}
