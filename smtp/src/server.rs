use std::{error::Error, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tracing::{Level, error};
use database::database::DatabaseClient;

use crate::{errors::SmtpErrorCode, smtp::HandleCurrentState};

const INITIAL_GREETING: &'static [u8] = b"220 Temp Mail Service Ready\n";
const TIMEOUT: Duration = Duration::from_secs(30);
pub const CLOSING_CONNECTION: &'static [u8] = b"221 Goodbye\n";

pub struct Server {
    connection: tokio::net::TcpStream,
    state_handler: HandleCurrentState,
    db: Arc<DatabaseClient>,
}

impl Server {
    pub async fn new(
        server_domain: impl AsRef<str>,
        connection: tokio::net::TcpStream,
        db: Arc<DatabaseClient>,
    ) -> Self {
        Self {
            connection,
            state_handler: HandleCurrentState::new(server_domain),
            db,
        }
    }

    pub async fn connection(mut self) -> Result<(), Box<dyn Error>> {
        let span = tracing::span!(Level::INFO, "MAIL");
        let _enter = span.enter();
        self.connection.write_all(INITIAL_GREETING).await?;
        tracing::info!("Greeted");
        let mut buffer: Vec<u8> = vec![0; 65536];
        let db = self.db.clone();

        loop {
            match timeout(TIMEOUT, self.connection.read(&mut buffer)).await {
                Ok(Ok(0)) => {
                    tracing::error!("Unexpected End of Stream without any data.");
                    break;
                }
                Ok(Ok(bytes)) => {
                    let message = match str::from_utf8(&buffer[0..bytes]) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::error!("Broken pipe, closing stream: {}", e);
                            return Err(Box::new(e));
                        }
                    };

                    match self.state_handler.process_smtp_command(message, &db).await {
                        Ok(response) => {
                            if response  != b"" {
                                self.connection.write_all(response).await?;
                            }
                            if response == CLOSING_CONNECTION {
                                tracing::warn!("Closing connection!");
                                break;
                            }
                        }
                        Err(err) => {
                             self.connection
                                .write_all(err.format_response().as_bytes())
                                .await?;
                            tracing::error!("Unexpected End of Stream, closing connection");
                            if err.code.as_code() >= SmtpErrorCode::SyntaxError.into() {
                                break;
                            }
                        }
                    }
                }
                Ok(Err(_)) => {
                    error!("Couldn't read stream");
                    break;
                }
                Err(_) => {
                    error!("Timeout Error: No data for 30 seconds. Closing!");
                    break;
                }
            }
        }
        Ok(())
    }
}
