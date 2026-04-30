use std::{mem::take, sync::Arc};

use database::database::DatabaseClient;
use database::quota::AddressLimits;
use database::webhooks::Webhooks;
use tracing::{error, info};

use crate::{
    errors::{SmtpErrorCode, SmtpResponseError}, is_email_valid, server::CLOSING_CONNECTION, types::{CurrentStates, Email, SMTPResult}, webhook::{extract_otp, send_webhook, Payload}
};

const MAX_EMAIL_SIZE: usize = 10_485_760;
const SUCCESS_RESPONSE: &'static [u8] = b"250 Ok\n";
const AUTH_OK: &'static [u8] = b"235 Ok\n";
const MAX_RECIPIENT_COUNT: usize = 100;
const DATA_READY_PROMPT: &'static [u8] = b"354 End data with <CR><LF>.<CR><LF>\n";

fn extract_subject(content: &str) -> String {
    content
        .lines()
        .find(|line| line.to_lowercase().starts_with("subject:"))
        .map(|s| s.trim_start_matches("Subject:").trim_start_matches("subject:").trim().to_string())
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
            "250-{server_domain} greets {server_domain}\n\
             250-SIZE {}\n\
             250 8BITMIME\n",
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
        db: &Arc<DatabaseClient>
    ) -> SMTPResult<'a, &[u8]> {
        let message = client_message.trim();

        if message.is_empty() {
            return Err(SmtpResponseError::new(&SmtpErrorCode::SyntaxError));
        }

        let mut message_parts = message.split_whitespace();
        let command = message_parts
            .next()
            .ok_or_else(|| SmtpResponseError::new(&SmtpErrorCode::SyntaxError))?
            .to_lowercase();

        let previous_state = std::mem::replace(&mut self.current_state, CurrentStates::Initial);

        match (command.as_str(), previous_state) {
            ("ehlo", CurrentStates::Initial) => {
                self.current_state = CurrentStates::Greeted;
                Ok(self.greeting_message.as_bytes())
            },
            ("helo", CurrentStates::Initial) => {
                self.current_state = CurrentStates::Greeted;
                Ok(self.greeting_message.as_bytes())
            },
            ("noop", _) | ("help", _) | ("info", _) | ("vrfy", _) | ("expn", _) => {
                tracing::warn!("RECIEVED: Unhandled commands");
                Ok(SUCCESS_RESPONSE)
            }
            ("rset", _) => {
                tracing::warn!("RECIEVED: Reset");
                self.current_state = CurrentStates::Initial;
                Ok(SUCCESS_RESPONSE)
            }
            ("auth", _) => {
                tracing::trace!("RECIEVED: auth");
                Ok(AUTH_OK)
            }
            ("mail", CurrentStates::Greeted) => {
                let sender = message_parts
                    .next()
                    .and_then(|s| s.strip_prefix("FROM:"))
                    .ok_or_else(|| SmtpResponseError::new(&SmtpErrorCode::InvalidParameters))?;

                if !is_email_valid(sender) {
                    return Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable));
                }

                tracing::trace!("RECIEVED MAIL from {}", sender);

                self.current_state = CurrentStates::AwaitingRecipient(Email {
                    sender: sender.to_string(),
                    ..Default::default()
                });
                Ok(SUCCESS_RESPONSE)
            }
            ("rcpt", CurrentStates::AwaitingRecipient(mut email)) => {
                if email.recipients.len() >= MAX_RECIPIENT_COUNT {
                    tracing::error!(
                        "ERROR: Max number of recipients reached, got: {}",
                        email.recipients.len()
                    );
                    return Err(SmtpResponseError::new(
                        &SmtpErrorCode::InsufficientSystemStorage,
                    ));
                }
                let receiver = message_parts
                    .next()
                    .and_then(|s| s.strip_prefix("TO:"))
                    .ok_or_else(|| SmtpResponseError::new(&SmtpErrorCode::InvalidParameters))?;

                if !is_email_valid(receiver) {
                    tracing::error!("ERROR: Invalid email: {}", receiver);
                    return Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable));
                }

                let receiver_clean = receiver.trim_start_matches('<').trim_end_matches('>');
                let receiver_domain = receiver_clean.split('@').nth(1).unwrap_or("");
                if receiver_domain != self.server_domain {
                    tracing::error!("ERROR: Relay denied for domain: {}", receiver_domain);
                    return Err(SmtpResponseError::new(&SmtpErrorCode::MailboxUnavailable));
                }

                email.recipients.push(receiver.to_string());
                tracing::trace!("RECIEVED: RCPT TO: {}", receiver);
                self.current_state = CurrentStates::AwaitingRecipient(email);
                Ok(SUCCESS_RESPONSE)
            }
            ("data", CurrentStates::AwaitingRecipient(email)) => {
                if email.recipients.is_empty() {
                    tracing::error!("ERROR: Recieved DATA with no recipients");
                    return Err(SmtpResponseError::new(&SmtpErrorCode::TransactionFailed));
                }
                self.current_state = CurrentStates::AwaitingData(email);
                Ok(DATA_READY_PROMPT)
            }
            ("quit", state) => match state {
                 CurrentStates::DataReceived(email) => {
                    tracing::info!(recipient_count = email.recipients.len(), "Mail received from {}, saving to database", email.sender);
                    tracing::debug!(subject = extract_subject(&email.content), size = email.size, "Email details");
                    tracing::trace!(content_preview = email.content.chars().take(200).collect::<String>(), "Email content preview");

                    // Process each recipient: check quota, save email, send webhook
                    for recipient in &email.recipients {
                        let mail_addr = recipient.trim_start_matches('<').trim_end_matches('>');

                        // Check quota and increment if allowed
                        let quota_ok = match AddressLimits::check_and_increment(db, mail_addr).await {
                            Ok(true) => true,
                            Ok(false) => {
                                tracing::warn!("Quota exceeded for recipient: {}", mail_addr);
                                false
                            }
                            Err(e) => {
                                tracing::error!("Failed to check quota for {}: {}", mail_addr, e);
                                true // Allow on quota check failure to not lose mail
                            }
                        };

                        if !quota_ok {
                            continue;
                        }

                        // Save email to database
                        let db_email = database::database::Email {
                            sender: email.sender.clone(),
                            recipients: vec![recipient.clone()],
                            content: email.content.clone(),
                            size: email.size,
                        };
                        match db.add_mail(db_email).await {
                            Ok(rows) => {
                                tracing::info!("Mail saved to database for {}, rows affected: {}", mail_addr, rows);
                                // Track analytics
                                let _ = db.increment_analytics("emails_received").await;
                            }
                            Err(e) => tracing::error!("Failed to save mail to database for {}: {}", mail_addr, e),
                        }

                        // Send webhook notification if configured
                        if let Ok(Some(webhook_url)) = Webhooks::get_webhook_address_for_mail(db, mail_addr).await {
                            let otp = extract_otp(&email.content);
                            let payload = Payload {
                                version: 1,
                                otp,
                                mail: mail_addr.to_string(),
                            };
                            match send_webhook(&webhook_url, &payload).await {
                                Ok(_) => info!("Webhook sent successfully for {}", mail_addr),
                                Err(e) => tracing::error!("Failed to send webhook for {}: {}", mail_addr, e),
                            }
                        }
                    }

                    Ok(CLOSING_CONNECTION)
                }
                _ => {
                    tracing::warn!("QUIT before DATA completed, discarding mail");
                    Ok(CLOSING_CONNECTION)
                }
            },
            (_, CurrentStates::AwaitingData(mut email)) => {
                email.size += client_message.len();
                if email.size > self.max_email_size {
                    tracing::error!("ERROR: Message size of 10MB exceeded. Closing!");
                    return Err(SmtpResponseError::new(
                        &SmtpErrorCode::MessageSizeExceedsLimit,
                    ));
                }
                email.content.push_str(client_message);

                let response =
                    if email.content.ends_with("\n.\n") || email.content.ends_with("\r\n.\r\n") {
                        self.current_state = CurrentStates::DataReceived(take(&mut email));
                        SUCCESS_RESPONSE
                    } else {
                        self.current_state = CurrentStates::AwaitingData(take(&mut email));
                        b""
                    };

                Ok(response)
            }
            _ => {
                error!("Unrecorgnized command");
                Err(SmtpResponseError::new(&SmtpErrorCode::CommandUnrecognized))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_subject() {
        assert_eq!(extract_subject("Subject: Hello World\n\nBody"), "Hello World");
        assert_eq!(extract_subject("subject: lowercase\n\nBody"), "lowercase");
        assert_eq!(extract_subject("No subject line\n\nBody"), "(no subject)");
        assert_eq!(extract_subject("From: sender@test.com\nSubject: My Subject\n\nBody"), "My Subject");
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
