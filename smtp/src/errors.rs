#[derive(Debug)]
pub enum SmtpErrorCode {
    SyntaxError,
    CommandUnrecognized,
    InvalidParameters,
    MailboxUnavailable,
    InsufficientSystemStorage,
    MessageSizeExceedsLimit,
    TransactionFailed,
}

impl SmtpErrorCode {
    pub fn as_code(&self) -> u16 {
        match self {
            SmtpErrorCode::SyntaxError => 500,
            SmtpErrorCode::CommandUnrecognized => 500,
            SmtpErrorCode::InvalidParameters => 501,
            SmtpErrorCode::MailboxUnavailable => 550,
            SmtpErrorCode::InsufficientSystemStorage => 452,
            SmtpErrorCode::MessageSizeExceedsLimit => 552,
            SmtpErrorCode::TransactionFailed => 554,
        }
    }

    pub fn as_message(&self) -> &str {
        match self {
            SmtpErrorCode::SyntaxError => "Syntax error, command unrecognized",
            SmtpErrorCode::CommandUnrecognized => "Command unrecognized",
            SmtpErrorCode::InvalidParameters => "Syntax error in parameters or arguments",
            SmtpErrorCode::MailboxUnavailable => "Requested action not taken (mailbox unavailable)",
            SmtpErrorCode::InsufficientSystemStorage => {
                "Requested action not taken (insufficient system storage)"
            }
            SmtpErrorCode::MessageSizeExceedsLimit => {
                "Requested action aborted (message size exceeds limit)"
            }
            SmtpErrorCode::TransactionFailed => "Transaction failed",
        }
    }
}

#[derive(Debug)]
pub struct SmtpResponseError<'a> {
    pub code: &'a SmtpErrorCode,
    message: &'a str,
}


impl<'a> SmtpResponseError<'a> {
    pub fn new(code: &'a SmtpErrorCode) -> Self {
        Self {
            code,
            message: code.as_message(),
        }
    }

    pub fn format_response(&self) -> String {
        format!("{} {}\n", self.code.as_code(), self.message)
    }
}


impl Into<u16> for SmtpErrorCode {
    fn into(self) -> u16 {
        self.as_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(SmtpErrorCode::SyntaxError.as_code(), 500);
        assert_eq!(SmtpErrorCode::CommandUnrecognized.as_code(), 500);
        assert_eq!(SmtpErrorCode::InvalidParameters.as_code(), 501);
        assert_eq!(SmtpErrorCode::MailboxUnavailable.as_code(), 550);
        assert_eq!(SmtpErrorCode::InsufficientSystemStorage.as_code(), 452);
        assert_eq!(SmtpErrorCode::MessageSizeExceedsLimit.as_code(), 552);
        assert_eq!(SmtpErrorCode::TransactionFailed.as_code(), 554);
    }

    #[test]
    fn test_error_messages() {
        assert_eq!(SmtpErrorCode::SyntaxError.as_message(), "Syntax error, command unrecognized");
        assert_eq!(SmtpErrorCode::CommandUnrecognized.as_message(), "Command unrecognized");
        assert_eq!(SmtpErrorCode::InvalidParameters.as_message(), "Syntax error in parameters or arguments");
        assert_eq!(SmtpErrorCode::MailboxUnavailable.as_message(), "Requested action not taken (mailbox unavailable)");
        assert_eq!(SmtpErrorCode::InsufficientSystemStorage.as_message(), "Requested action not taken (insufficient system storage)");
        assert_eq!(SmtpErrorCode::MessageSizeExceedsLimit.as_message(), "Requested action aborted (message size exceeds limit)");
        assert_eq!(SmtpErrorCode::TransactionFailed.as_message(), "Transaction failed");
    }

    #[test]
    fn test_format_response() {
        let err = SmtpResponseError::new(&SmtpErrorCode::SyntaxError);
        assert_eq!(err.format_response(), "500 Syntax error, command unrecognized\n");
    }

    #[test]
    fn test_into_u16() {
        let code: u16 = SmtpErrorCode::MailboxUnavailable.into();
        assert_eq!(code, 550);
    }
}
