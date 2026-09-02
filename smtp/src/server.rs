use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tracing::error;
use database::database::DatabaseClient;

use crate::{
    errors::SmtpErrorCode,
    metrics::SmtpMetrics,
    smtp::HandleCurrentState,
    types::SmtpReplyEvent,
};

const INITIAL_GREETING: &'static [u8] = b"220 Temp Mail Service Ready\n";
const TIMEOUT: Duration = Duration::from_secs(30);
pub const CLOSING_CONNECTION: &'static [u8] = b"221 Goodbye\n";

pub struct Server {
    connection: tokio::net::TcpStream,
    state_handler: HandleCurrentState,
    db: Arc<DatabaseClient>,
    metrics: Arc<SmtpMetrics>,
    greeting_start: Instant,
}

impl Server {
    pub async fn new(
        server_domain: impl AsRef<str>,
        connection: tokio::net::TcpStream,
        db: Arc<DatabaseClient>,
        metrics: Arc<SmtpMetrics>,
        greeting_start: Instant,
    ) -> Self {
        Self {
            connection,
            state_handler: HandleCurrentState::new(server_domain),
            db,
            metrics,
            greeting_start,
        }
    }

    #[tracing::instrument(name = "MAIL", skip_all)]
    pub async fn connection(mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let _inflight = self.metrics.session();
        self.connection.write_all(INITIAL_GREETING).await?;
        self.metrics.record_greeting(self.greeting_start.elapsed());
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
                    let command_start = Instant::now();
                    let message = match std::str::from_utf8(&buffer[0..bytes]) {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::error!("Broken pipe, closing stream: {}", e);
                            return Err(Box::new(e));
                        }
                    };

                    match self.state_handler.process_smtp_command(message, &db).await {
                        Ok(reply) => {
                            if !reply.bytes.is_empty() {
                                self.connection.write_all(reply.bytes).await?;
                            }
                            match reply.event {
                                SmtpReplyEvent::DataAccepted => {
                                    self.metrics.record_data_accept(command_start.elapsed());
                                }
                                SmtpReplyEvent::Closing => {
                                    tracing::warn!("Closing connection!");
                                    break;
                                }
                                SmtpReplyEvent::None => {}
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
