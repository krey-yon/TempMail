use dotenv::dotenv;
use smtp::start_smtp_server;
use tracing::error;

fn main() {
    tracing_subscriber::fmt::init();

    if dotenv().is_err(){
        error!("Failed to load .env file");
    }

    let addr:std::net::SocketAddr = "0.0.0.0:25".parse().unwrap();
    let domain = String::from("mail.jasscodes.in");

    if let Err(e) = start_smtp_server(addr, domain){
         tracing::error!("Error starting server: {}", e);
        eprintln!("Error starting server: {}", e);
    }
}
