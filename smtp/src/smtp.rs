use std::sync::Arc;

use database::database::{DatabaseClient, MailDeliveryError};
use tracing::{error, info};

use crate::{
    errors::{SmtpErrorCode, SmtpResponseError},
    is_email_valid,
    server::CLOSING_CONNECTION,
    types::{CurrentStates, Email, SMTPResult, SmtpReply},
    webhook::{extract_otp, send_webhook, Payload},
};

const MAX_EMAIL_SIZE: usize = 10_485_760;
const SUCCESS_RESPONSE: &[u8] = b"250 Ok\r\n";
const MAX_RECIPIENT_COUNT: usize = 100;
const DATA_READY_PROMPT: &[u8] = b"354 End data with <CR><LF>.<CR><LF>\r\n";

fn extract_subject(content: &str) -> String {
    content
        .lines()
        .find(|line| line.to_lowercase().starts_with("subject:"))
        .map(|s| {
            s.trim_start_matches("Subject:")
                .trim_start_matches("subject:")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "(no subject)".to_string())
}

pub struct HandleCurrentState {
    current_state: CurrentStates,
    greeting_message: String,
    max_email_size: usize,
    server_domain: String,
}

impl HandleCurrentState {
    pub fn new(server_domain: impl AsRef<str>) -> Self {
        let server_domain = server_domain.as_ref();
        let greeting_message = format!(
            "250-{server_domain} greets {server_domain}\r\n250-SIZE {}\r\n250 8BITMIME\r\n",
            MAX_EMAIL_SIZE
        );
        Self {
            current_state: CurrentStates::Initial,
            greeting_message,
            max_email_size: MAX_EMAIL_SIZE,
            server_domain: server_domain.to_string(),
        }
    }

    pub async fn process_smtp_command<'a>(
        &mut self,
        client_message: &str,
        db: &Arc<DatabaseClient>,
    ) -> SMTPResult<'a, SmtpReply<'_>> {
        // Body lines, including blank lines or words like QUIT, are data.
        let previous_state = std::mem::replace(&mut self.current_state, CurrentStates::Initial);
        if let CurrentStates::AwaitingData(email) = previous_state {
            return self.receive_data(client_message, email, db).await;
        }
        self.current_state = previous_state;
        let mut parts = client_message.split_whitespace();
        let command = parts.next().unwrap_or("").to_ascii_lowercase();
        if command.is_empty() {
            return Err(SmtpResponseError::new(&SmtpErrorCode::SyntaxError));
        }
        match command.as_str() {
            "quit" => return Ok(SmtpReply::closing(CLOSING_CONNECTION)),
            "noop" => return Ok(SmtpReply::none(SUCCESS_RESPONSE)),
            "rset" => {
                self.current_state = CurrentStates::Greeted;
                return Ok(SmtpReply::none(SUCCESS_RESPONSE));
            }
            _ => {}
        }
        let state = std::mem::replace(&mut self.current_state, CurrentStates::Initial);
        match (command.as_str(), state) {
            ("ehlo" | "helo", _) => {
                self.current_state = CurrentStates::Greeted;
                Ok(SmtpReply::none(self.greeting_message.as_bytes()))
            }
            ("mail", CurrentStates::Greeted) => {
                let sender = parts
                    .next()
                    .and_then(|s| s.strip_prefix("FROM:").or_else(|| s.strip_prefix("from:")))
                    .ok_or_else(|| SmtpResponseError::new(&SmtpErrorCode::InvalidParameters))?;
                if !is_email_valid(sender) && sender != "<>" {
                    return Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable));
                }
                self.current_state = CurrentStates::AwaitingRecipient(Email {
                    sender: sender.to_string(),
                    ..Default::default()
                });
                Ok(SmtpReply::none(SUCCESS_RESPONSE))
            }
            ("rcpt", CurrentStates::AwaitingRecipient(mut email)) => {
                if email.recipients.len() >= MAX_RECIPIENT_COUNT {
                    self.current_state = CurrentStates::AwaitingRecipient(email);
                    return Err(SmtpResponseError::new(
                        &SmtpErrorCode::InsufficientSystemStorage,
                    ));
                }
                let receiver = parts
                    .next()
                    .and_then(|s| s.strip_prefix("TO:").or_else(|| s.strip_prefix("to:")))
                    .ok_or_else(|| SmtpResponseError::new(&SmtpErrorCode::InvalidParameters))?;
                let address = receiver.trim_start_matches('<').trim_end_matches('>');
                if !is_email_valid(address)
                    || address.rsplit('@').next() != Some(self.server_domain.as_str())
                {
                    self.current_state = CurrentStates::AwaitingRecipient(email);
                    return Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable));
                }
                let mailbox = db.get_active_mailbox(address).await;
                match mailbox {
                    Ok(Some(recipient)) => {
                        if !email.recipients.contains(&recipient) {
                            email.recipients.push(recipient);
                        }
                        self.current_state = CurrentStates::AwaitingRecipient(email);
                        Ok(SmtpReply::none(SUCCESS_RESPONSE))
                    }
                    Ok(None) => {
                        self.current_state = CurrentStates::AwaitingRecipient(email);
                        Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable))
                    }
                    Err(e) => {
                        error!("Recipient lookup failed: {}", e);
                        self.current_state = CurrentStates::AwaitingRecipient(email);
                        Err(SmtpResponseError::new(&SmtpErrorCode::TemporaryFailure))
                    }
                }
            }
            ("data", CurrentStates::AwaitingRecipient(email)) => {
                if email.recipients.is_empty() {
                    self.current_state = CurrentStates::AwaitingRecipient(email);
                    return Err(SmtpResponseError::new(&SmtpErrorCode::TransactionFailed));
                }
                self.current_state = CurrentStates::AwaitingData(email);
                Ok(SmtpReply::none(DATA_READY_PROMPT))
            }
            (_, state) => {
                self.current_state = state;
                Err(SmtpResponseError::new(&SmtpErrorCode::CommandUnrecognized))
            }
        }
    }

    async fn receive_data<'a>(
        &mut self,
        line: &str,
        mut email: Email,
        db: &Arc<DatabaseClient>,
    ) -> SMTPResult<'a, SmtpReply<'_>> {
        if line != ".\r\n" && line != ".\n" {
            email.size += line.len();
            if email.size > self.max_email_size {
                self.current_state = CurrentStates::Greeted;
                return Err(SmtpResponseError::new(
                    &SmtpErrorCode::MessageSizeExceedsLimit,
                ));
            }
            // SMTP dot-stuffing escapes a leading dot in body lines.
            email.content.push_str(if line.starts_with("..") {
                &line[1..]
            } else {
                line
            });
            self.current_state = CurrentStates::AwaitingData(email);
            return Ok(SmtpReply::none(b""));
        }
        self.current_state = CurrentStates::Greeted;
        let data = database::database::Email {
            sender: email.sender.clone(),
            recipients: email.recipients.iter().map(|r| r.address.clone()).collect(),
            content: email.content.clone(),
            size: email.size,
        };
        let notifications = match db.deliver_mail(data, &email.recipients).await {
            Ok(notifications) => notifications,
            Err(e) => {
                error!("SMTP delivery failed: {}", e);
                let code = match e.downcast_ref::<MailDeliveryError>() {
                    Some(MailDeliveryError::MailboxUnavailable) => {
                        &SmtpErrorCode::MailboxUnavailable
                    }
                    Some(MailDeliveryError::QuotaExceeded) => {
                        &SmtpErrorCode::InsufficientSystemStorage
                    }
                    None => &SmtpErrorCode::TemporaryFailure,
                };
                return Err(SmtpResponseError::new(code));
            }
        };
        info!(
            recipient_count = notifications.len(),
            subject = extract_subject(&email.content),
            "Mail stored before DATA acknowledgement"
        );
        // Webhook URLs were captured under the same ownership lock as delivery.
        // Slow or failed webhooks must not delay SMTP's successful response.
        let otp = extract_otp(&email.content);
        for (address, webhook) in notifications {
            if let Some(url) = webhook {
                let payload = Payload {
                    version: 1,
                    otp: otp.clone(),
                    mail: address,
                };
                tokio::spawn(async move {
                    if let Err(e) = send_webhook(&url, &payload).await {
                        error!("Webhook delivery failed: {}", e);
                    }
                });
            }
        }
        Ok(SmtpReply::data_accepted(SUCCESS_RESPONSE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_subject() {
        assert_eq!(
            extract_subject("Subject: Hello World\n\nBody"),
            "Hello World"
        );
        assert_eq!(extract_subject("subject: lowercase\n\nBody"), "lowercase");
        assert_eq!(extract_subject("No subject line\n\nBody"), "(no subject)");
        assert_eq!(
            extract_subject("From: sender@test.com\nSubject: My Subject\n\nBody"),
            "My Subject"
        );
    }

    #[test]
    fn test_max_email_size() {
        assert_eq!(MAX_EMAIL_SIZE, 10_485_760); // 10MB
    }

    #[test]
    fn test_max_recipient_count() {
        assert_eq!(MAX_RECIPIENT_COUNT, 100);
    }
}
