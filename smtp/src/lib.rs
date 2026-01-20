use std::{error::Error, net::SocketAddr, sync::Arc, time::Duration};

use tokio::{net::TcpListener, time::timeout};
use tracing::info;

use crate::server::Server;
pub mod server;
mod smtp;
mod types;
mod errors;


#[tokio::main]
pub async fn start_smtp_server(addr: SocketAddr, domain: String) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(addr).await?;
    let domain = Arc::new(domain);

    info!("Server started on Port: {}", addr);

    loop {
        let (stream,_addr) = listener.accept().await?;
        let domain = Arc::new(&domain);

        // used to make the tasks run in only single thread: TODO: Check if we really need this impl
         tokio::task::LocalSet::new()
            .run_until(async move {
                tracing::info!("Ping received on SMTP Server");
                let smtp: Server = Server::new(domain.as_str(), stream).await?;

            })
            .await;
    }

    Ok(())
}