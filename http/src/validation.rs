use regex::Regex;
use std::fmt;

#[derive(Debug)]
pub enum ValidationError {
    InvalidLength,
    InvalidCharacters,
    InvalidFormat,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidLength => write!(f, "Username must be 3-32 characters"),
            Self::InvalidCharacters => {
                write!(
                    f,
                    "Username must contain only alphanumeric, hyphens, or underscores"
                )
            }
            Self::InvalidFormat => write!(f, "Invalid email address format"),
        }
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug)]
#[allow(dead_code)]
pub enum DatabaseError {
    DuplicateAddress,
    NotFound,
    ConnectionError,
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::DuplicateAddress => write!(f, "Email address already exists"),
            Self::NotFound => write!(f, "Email address not found"),
            Self::ConnectionError => write!(f, "Database connection error"),
        }
    }
}

impl std::error::Error for DatabaseError {}

pub struct UsernameValidator;

impl UsernameValidator {
    pub fn validate(username: &str) -> Result<String, ValidationError> {
        // Length check
        if username.len() < 3 || username.len() > 32 {
            return Err(ValidationError::InvalidLength);
        }

        // Character check
        let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
        if !re.is_match(username) {
            return Err(ValidationError::InvalidCharacters);
        }

        // Convert to lowercase
        Ok(username.to_lowercase())
    }

    pub fn validate_email_format(email: &str) -> Result<(), ValidationError> {
        let re = Regex::new(r"^[a-zA-Z0-9_-]+@voidmail\.io$").unwrap();
        if !re.is_match(email) {
            return Err(ValidationError::InvalidFormat);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_username() {
        assert!(UsernameValidator::validate("vikas").is_ok());
        assert!(UsernameValidator::validate("test-user").is_ok());
        assert!(UsernameValidator::validate("user_123").is_ok());
    }

    #[test]
    fn test_username_too_short() {
        assert!(matches!(
            UsernameValidator::validate("ab"),
            Err(ValidationError::InvalidLength)
        ));
    }

    #[test]
    fn test_username_too_long() {
        let long_name = "a".repeat(33);
        assert!(matches!(
            UsernameValidator::validate(&long_name),
            Err(ValidationError::InvalidLength)
        ));
    }

    #[test]
    fn test_username_invalid_characters() {
        assert!(matches!(
            UsernameValidator::validate("user@test"),
            Err(ValidationError::InvalidCharacters)
        ));
        assert!(matches!(
            UsernameValidator::validate("user.test"),
            Err(ValidationError::InvalidCharacters)
        ));
    }

    #[test]
    fn test_username_lowercase_conversion() {
        let result = UsernameValidator::validate("ViKaS").unwrap();
        assert_eq!(result, "vikas");
    }

    #[test]
    fn test_valid_email_format() {
        assert!(UsernameValidator::validate_email_format("vikas@voidmail.io").is_ok());
    }

    #[test]
    fn test_invalid_email_format() {
        assert!(matches!(
            UsernameValidator::validate_email_format("vikas@gmail.com"),
            Err(ValidationError::InvalidFormat)
        ));
    }
}
