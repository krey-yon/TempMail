pub mod database;
pub mod quota;
pub mod webhooks;
pub mod clear_old_mails;

pub use database::{DatabaseClient, Email, MailRow};
pub use quota::AddressLimits;
pub use webhooks::Webhooks;
pub use clear_old_mails::clear_old_mails;

#[cfg(test)]
mod tests {
    use crate::database::{Email, MailRow};

    #[test]
    fn test_email_struct() {
        let email = Email {
            sender: "sender@example.com".to_string(),
            recipients: vec!["recipient@example.com".to_string()],
            content: "Test content".to_string(),
            size: 12,
        };
        assert_eq!(email.sender, "sender@example.com");
        assert_eq!(email.recipients.len(), 1);
        assert_eq!(email.content, "Test content");
        assert_eq!(email.size, 12);
    }

    #[test]
    fn test_email_multiple_recipients() {
        let email = Email {
            sender: "sender@example.com".to_string(),
            recipients: vec![
                "recipient1@example.com".to_string(),
                "recipient2@example.com".to_string(),
            ],
            content: "Test".to_string(),
            size: 4,
        };
        assert_eq!(email.recipients.len(), 2);
    }

    #[test]
    fn test_mail_row_clone() {
        let row = MailRow {
            id: 1,
            date: "2024-01-01".to_string(),
            sender: "sender@example.com".to_string(),
            recipients: "recipient@example.com".to_string(),
            data: "Email data".to_string(),
        };
        let cloned = row.clone();
        assert_eq!(cloned.id, 1);
        assert_eq!(cloned.sender, "sender@example.com");
    }

    #[test]
    fn test_email_default() {
        let email = Email::default();
        assert!(email.sender.is_empty());
        assert!(email.recipients.is_empty());
        assert_eq!(email.size, 0);
    }

    #[test]
    fn test_mail_row_default() {
        let row = MailRow::default();
        assert_eq!(row.id, 0);
        assert!(row.sender.is_empty());
    }
}
