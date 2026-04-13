use crate::errors::SmtpResponseError;

#[derive(Default, Clone, Debug)]
pub struct Email {
    pub sender: String,
    pub recipients: Vec<String>,
    pub content: String,
    pub size: usize,
}

pub enum CurrentStates {
    Initial,
    Greeted,
    AwaitingRecipient(Email),
    AwaitingData(Email),
    DataReceived(Email),
}


pub type SMTPResult<'a, T> = Result<T, SmtpResponseError<'a>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_default() {
        let email = Email::default();
        assert!(email.sender.is_empty());
        assert!(email.recipients.is_empty());
        assert!(email.content.is_empty());
        assert_eq!(email.size, 0);
    }

    #[test]
    fn test_email_clone() {
        let email = Email {
            sender: "test@example.com".to_string(),
            recipients: vec!["recipient@example.com".to_string()],
            content: "Hello".to_string(),
            size: 5,
        };
        let cloned = email.clone();
        assert_eq!(cloned.sender, email.sender);
        assert_eq!(cloned.recipients, email.recipients);
        assert_eq!(cloned.content, email.content);
        assert_eq!(cloned.size, email.size);
    }
}
