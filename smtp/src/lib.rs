use std::{error::Error, net::SocketAddr, sync::Arc, time::Duration};

use tokio::{net::TcpListener, time::timeout};
use tracing::info;

use crate::server::Server;
mod errors;
pub mod server;
mod smtp;
mod types;

#[tokio::main]
pub async fn start_smtp_server(addr: SocketAddr, domain: String) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(addr).await?;
    let domain = Arc::new(domain);

    info!("Server started on Port: {}", addr);

    loop {
        let (stream, _addr) = listener.accept().await?;
        let domain = Arc::new(&domain);

        // used to make the tasks run in only single thread: TODO: Check if we really need this impl
        tokio::task::LocalSet::new()
            .run_until(async move {
                tracing::info!("Ping received on SMTP Server");
                let smtp = Server::new(domain.as_str(), stream).await?;
                match timeout(Duration::from_secs(300), smtp.connection()).await {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(e)) => Err(e),
                    Err(e) => Err(Box::new(e) as Box<dyn Error>),
                }
            })
            .await
            .ok();
    }
}

pub fn is_email_valid(email: &str) -> bool {
    email.contains('@') && !email.contains("..") && email.len() < 254
}
