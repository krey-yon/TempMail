use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use database::database::DatabaseClient;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    time::timeout,
};
use tracing::error;

use crate::{
    errors::SmtpErrorCode, metrics::SmtpMetrics, smtp::HandleCurrentState, types::SmtpReplyEvent,
};

const INITIAL_GREETING: &'static [u8] = b"220 Temp Mail Service Ready\r\n";
const TIMEOUT: Duration = Duration::from_secs(30);
pub const CLOSING_CONNECTION: &'static [u8] = b"221 Goodbye\r\n";

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
        let (read_half, mut write_half) = self.connection.into_split();
        let mut reader = BufReader::new(read_half);
        let mut buffer = Vec::new();
        let db = self.db.clone();

        loop {
            buffer.clear();
            // Bound one line and buffer partial commands across TCP reads.
            let mut limited = (&mut reader).take(10_485_762);
            match timeout(TIMEOUT, limited.read_until(b'\n', &mut buffer)).await {
                Ok(Ok(0)) => {
                    tracing::error!("Unexpected End of Stream without any data.");
                    break;
                }
                Ok(Ok(bytes)) => {
                    if buffer.len() >= 10_485_762 {
                        write_half
                            .write_all(b"552 Message size exceeds limit\r\n")
                            .await?;
                        break;
                    }
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
                                write_half.write_all(reply.bytes).await?;
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
                            write_half
                                .write_all(err.format_response().as_bytes())
                                .await?;
                            tracing::warn!(code = err.code.as_code(), "SMTP command rejected");
                            if matches!(err.code, SmtpErrorCode::MessageSizeExceedsLimit) {
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};

    async fn session(
        db: Arc<DatabaseClient>,
    ) -> (BufReader<TcpStream>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let server = Server::new(
                "xelio.me",
                stream,
                db,
                Arc::new(SmtpMetrics::new()),
                Instant::now(),
            )
            .await;
            server.connection().await.unwrap();
        });
        let mut client = BufReader::new(client);
        reply(&mut client, "220").await;
        (client, task)
    }

    async fn reply(client: &mut BufReader<TcpStream>, expected: &str) {
        loop {
            let mut line = String::new();
            timeout(Duration::from_secs(5), client.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert!(
                line.starts_with(expected),
                "expected {expected}, got {line:?}"
            );
            if line.as_bytes().get(3) != Some(&b'-') {
                break;
            }
        }
    }

    async fn expire(db: &DatabaseClient, address: &str) {
        db.pool.get().await.unwrap().execute(
            "UPDATE email_addresses SET created_at = ((clock_timestamp() AT TIME ZONE 'UTC') - INTERVAL '24 hours')::text WHERE address = $1",
            &[&address],
        ).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an isolated PostgreSQL database configured through DB_* variables"]
    async fn smtp_rejects_expired_and_saves_before_quit_without_crossing_owners() {
        assert!(
            std::env::var("DB_NAME").unwrap().ends_with("_test"),
            "use an isolated test database"
        );
        let db = Arc::new(DatabaseClient::connect().await.unwrap());
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let username = format!("smtp{suffix}");
        let first = db.create_email_address(&username).await.unwrap();
        let (mut client, server) = session(db.clone()).await;
        // Several commands in one TCP write, then a fragmented command.
        client
            .get_mut()
            .write_all(b"EHLO example.org\r\nMAIL FROM:<>\r\nRCPT TO:<unknown@xelio.me>\r\nRC")
            .await
            .unwrap();
        reply(&mut client, "250").await;
        reply(&mut client, "250").await;
        reply(&mut client, "550").await;
        client
            .get_mut()
            .write_all(
                format!(
                    "PT TO:<{}>\r\nDATA\r\nSubject: first\r\n\r\nQUIT\r\n..dot\r\n.\r\n",
                    first.address
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        reply(&mut client, "250").await;
        reply(&mut client, "354").await;
        reply(&mut client, "250").await;
        // Disconnect without QUIT: acknowledged mail must already be stored.
        drop(client);
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        let mails = db
            .get_mails_by_recipient(&first.address, first.access_token.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(mails.len(), 1);
        assert!(mails[0].data.contains("\r\nQUIT\r\n.dot\r\n"));

        let (mut client, server) = session(db.clone()).await;
        client.get_mut().write_all(format!("EHLO example.org\r\nMAIL FROM:<sender@example.org>\r\nRCPT TO:<{}>\r\nDATA\r\n", first.address).as_bytes()).await.unwrap();
        reply(&mut client, "250").await;
        reply(&mut client, "250").await;
        reply(&mut client, "250").await;
        reply(&mut client, "354").await;
        expire(&db, &first.address).await;
        let second = db.create_email_address(&username).await.unwrap();
        client
            .get_mut()
            .write_all(b"Subject: old ownership\r\n\r\nsecret\r\n.\r\n")
            .await
            .unwrap();
        reply(&mut client, "550").await;
        assert!(db
            .get_mails_by_recipient(&second.address, second.access_token.as_deref().unwrap())
            .await
            .unwrap()
            .is_empty());
        // The connection can start a new transaction for the reclaimed owner.
        client.get_mut().write_all(format!("MAIL FROM:<sender@example.org>\r\nRCPT TO:<{}>\r\nDATA\r\nSubject: new ownership\r\n\r\nhello\r\n.\r\n", second.address).as_bytes()).await.unwrap();
        reply(&mut client, "250").await;
        reply(&mut client, "250").await;
        reply(&mut client, "354").await;
        reply(&mut client, "250").await;
        client.get_mut().write_all(b"QUIT\r\n").await.unwrap();
        reply(&mut client, "221").await;
        drop(client);
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.get_mails_by_recipient(&second.address, second.access_token.as_deref().unwrap())
                .await
                .unwrap()
                .len(),
            1
        );

        expire(&db, &second.address).await;
        let (mut client, server) = session(db.clone()).await;
        client.get_mut().write_all(format!("EHLO example.org\r\nMAIL FROM:<sender@example.org>\r\nRCPT TO:<{}>\r\nQUIT\r\n", second.address).as_bytes()).await.unwrap();
        reply(&mut client, "250").await;
        reply(&mut client, "250").await;
        reply(&mut client, "550").await;
        reply(&mut client, "221").await;
        drop(client);
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        db.delete_old_email_addresses().await.unwrap();
    }
}
