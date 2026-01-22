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
