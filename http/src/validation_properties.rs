// Feature: custom-tempmail-management, Property 3: Username Validation Rejection
// **Validates: Requirements 1.2, 1.3, 1.4, 6.6**
//
// Property: For any string that violates username rules (length < 3, length > 32,
// or contains non-alphanumeric/hyphen/underscore characters), attempting to create
// an email address should fail with a 400 Bad Request error.

#[cfg(test)]
mod username_validation_rejection_tests {
    use crate::validation::{UsernameValidator, ValidationError};
    use proptest::prelude::*;

    // Strategy for generating invalid usernames by length (too short)
    fn invalid_username_too_short() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-zA-Z0-9_-]{0,2}").unwrap()
    }

    // Strategy for generating invalid usernames by length (too long)
    fn invalid_username_too_long() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-zA-Z0-9_-]{33,50}").unwrap()
    }

    // Strategy for generating usernames with invalid characters
    fn invalid_username_bad_chars() -> impl Strategy<Value = String> {
        // Generate strings that contain at least one invalid character
        // Valid chars are: a-z, A-Z, 0-9, -, _
        // Invalid chars include: @, ., !, #, $, %, space, etc.
        prop::string::string_regex("[a-zA-Z0-9_-]{0,10}[@.!#$%^&*() +=/\\\\|<>?;:'\",\\[\\]{}~`][a-zA-Z0-9_-]{0,10}").unwrap()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: custom-tempmail-management, Property 3: Username Validation Rejection
        #[test]
        fn test_username_too_short_rejected(username in invalid_username_too_short()) {
            // Only test usernames that are actually too short (< 3 chars)
            prop_assume!(username.len() < 3);
            
            let result = UsernameValidator::validate(&username);
            
            // Should fail with InvalidLength error
            prop_assert!(result.is_err(), "Username '{}' with length {} should be rejected", username, username.len());
            prop_assert!(
                matches!(result.unwrap_err(), ValidationError::InvalidLength),
                "Username '{}' should fail with InvalidLength error",
                username
            );
        }

        // Feature: custom-tempmail-management, Property 3: Username Validation Rejection
        #[test]
        fn test_username_too_long_rejected(username in invalid_username_too_long()) {
            // Only test usernames that are actually too long (> 32 chars)
            prop_assume!(username.len() > 32);
            
            let result = UsernameValidator::validate(&username);
            
            // Should fail with InvalidLength error
            prop_assert!(result.is_err(), "Username '{}' with length {} should be rejected", username, username.len());
            prop_assert!(
                matches!(result.unwrap_err(), ValidationError::InvalidLength),
                "Username '{}' should fail with InvalidLength error",
                username
            );
        }

        // Feature: custom-tempmail-management, Property 3: Username Validation Rejection
        #[test]
        fn test_username_invalid_characters_rejected(username in invalid_username_bad_chars()) {
            // Ensure the username has valid length but contains invalid characters
            prop_assume!(username.len() >= 3 && username.len() <= 32);
            
            // Verify it actually contains invalid characters
            let has_invalid_chars = username.chars().any(|c| {
                !c.is_alphanumeric() && c != '-' && c != '_'
            });
            prop_assume!(has_invalid_chars);
            
            let result = UsernameValidator::validate(&username);
            
            // Should fail with InvalidCharacters error
            prop_assert!(result.is_err(), "Username '{}' with invalid characters should be rejected", username);
            prop_assert!(
                matches!(result.unwrap_err(), ValidationError::InvalidCharacters),
                "Username '{}' should fail with InvalidCharacters error",
                username
            );
        }

        // Feature: custom-tempmail-management, Property 3: Username Validation Rejection
        // Combined test: any invalid username should be rejected
        #[test]
        fn test_any_invalid_username_rejected(username in ".*") {
            // Filter to only invalid usernames
            let is_invalid = username.len() < 3 
                || username.len() > 32 
                || username.chars().any(|c| !c.is_alphanumeric() && c != '-' && c != '_');
            
            prop_assume!(is_invalid);
            
            let result = UsernameValidator::validate(&username);
            
            // Should fail with some validation error
            prop_assert!(
                result.is_err(),
                "Invalid username '{}' (len={}) should be rejected",
                username,
                username.len()
            );
            
            // Verify the error is one of the expected types
            let err = result.unwrap_err();
            prop_assert!(
                matches!(err, ValidationError::InvalidLength | ValidationError::InvalidCharacters),
                "Username '{}' should fail with InvalidLength or InvalidCharacters error, got: {:?}",
                username,
                err
            );
        }
    }
}

// Feature: custom-tempmail-management, Property 4: Case Normalization
// **Validates: Requirements 1.6**
//
// Property: For any username with mixed case letters, the stored email address
// should have the username portion converted to lowercase, and subsequent lookups
// should be case-insensitive.

#[cfg(test)]
mod case_normalization_tests {
    use crate::validation::UsernameValidator;
    use proptest::prelude::*;

    // Strategy for generating valid usernames with mixed case
    fn valid_username_mixed_case() -> impl Strategy<Value = String> {
        // Generate usernames with at least one uppercase letter
        prop::string::string_regex("[a-zA-Z0-9_-]{3,32}").unwrap()
            .prop_filter("Must contain at least one letter", |s| {
                s.chars().any(|c| c.is_alphabetic())
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        // Feature: custom-tempmail-management, Property 4: Case Normalization
        #[test]
        fn test_username_converted_to_lowercase(username in valid_username_mixed_case()) {
            // Ensure username is valid length
            prop_assume!(username.len() >= 3 && username.len() <= 32);
            
            // Ensure username contains only valid characters
            let is_valid = username.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_');
            prop_assume!(is_valid);
            
            let expected_lowercase = username.to_lowercase();
            let result = UsernameValidator::validate(&username);
            
            // Should succeed
            prop_assert!(
                result.is_ok(),
                "Valid username '{}' should be accepted",
                username
            );
            
            let normalized = result.unwrap();
            
            // The normalized username should be lowercase
            prop_assert_eq!(
                &normalized,
                &expected_lowercase,
                "Username '{}' should be converted to lowercase '{}'",
                username,
                expected_lowercase
            );
            
            // Verify no uppercase letters remain
            prop_assert!(
                !normalized.chars().any(|c| c.is_uppercase()),
                "Normalized username '{}' should not contain uppercase letters",
                normalized
            );
        }

        // Feature: custom-tempmail-management, Property 4: Case Normalization
        // Test that different case variations of the same username normalize to the same value
        #[test]
        fn test_case_insensitive_normalization(username in "[a-z0-9_-]{3,32}") {
            // Create variations with different casing
            let lowercase = username.to_lowercase();
            let uppercase = username.to_uppercase();
            let mixed = username.chars().enumerate().map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_uppercase().next().unwrap_or(c)
                } else {
                    c.to_lowercase().next().unwrap_or(c)
                }
            }).collect::<String>();
            
            // All variations should normalize to the same lowercase value
            let result_lower = UsernameValidator::validate(&lowercase);
            let result_upper = UsernameValidator::validate(&uppercase);
            let result_mixed = UsernameValidator::validate(&mixed);
            
            prop_assert!(result_lower.is_ok());
            prop_assert!(result_upper.is_ok());
            prop_assert!(result_mixed.is_ok());
            
            let normalized_lower = result_lower.unwrap();
            let normalized_upper = result_upper.unwrap();
            let normalized_mixed = result_mixed.unwrap();
            
            // All should be equal to the lowercase version
            prop_assert_eq!(
                &normalized_lower,
                &lowercase,
                "Lowercase username should remain lowercase"
            );
            prop_assert_eq!(
                &normalized_upper,
                &lowercase,
                "Uppercase username '{}' should normalize to lowercase '{}'",
                uppercase,
                lowercase
            );
            prop_assert_eq!(
                &normalized_mixed,
                &lowercase,
                "Mixed case username '{}' should normalize to lowercase '{}'",
                mixed,
                lowercase
            );
        }
    }
}
