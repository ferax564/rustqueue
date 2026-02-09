//! Shared bearer-token authentication utilities.
//!
//! This module centralizes token extraction and validation so all protocol
//! surfaces (HTTP middleware, TCP handshake, and embedding integrations) use
//! the same rules.

/// Errors returned while validating bearer token credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenValidationError {
    MissingAuthorizationHeader,
    InvalidAuthorizationScheme,
    MissingBearerToken,
    InvalidBearerToken,
}

impl TokenValidationError {
    /// Human-readable message aligned with RustQueue API error responses.
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingAuthorizationHeader => "Missing Authorization header",
            Self::InvalidAuthorizationScheme => "Authorization header must use Bearer scheme",
            Self::MissingBearerToken => "Missing bearer token",
            Self::InvalidBearerToken => "Invalid bearer token",
        }
    }
}

/// Validate a full HTTP `Authorization` header against configured tokens.
pub fn validate_bearer_header(
    header: Option<&str>,
    valid_tokens: &[String],
) -> Result<(), TokenValidationError> {
    let header = header.ok_or(TokenValidationError::MissingAuthorizationHeader)?;
    let token = extract_bearer_token(header)?;
    validate_bearer_token(token, valid_tokens)
}

/// Extract the token from an `Authorization: Bearer <token>` header value.
pub fn extract_bearer_token(header: &str) -> Result<&str, TokenValidationError> {
    let token = header
        .strip_prefix("Bearer ")
        .ok_or(TokenValidationError::InvalidAuthorizationScheme)?;
    if token.is_empty() {
        return Err(TokenValidationError::MissingBearerToken);
    }
    Ok(token)
}

/// Validate a raw bearer token string against configured tokens.
pub fn validate_bearer_token(
    token: &str,
    valid_tokens: &[String],
) -> Result<(), TokenValidationError> {
    if token.is_empty() {
        return Err(TokenValidationError::MissingBearerToken);
    }
    if valid_tokens.iter().any(|t| t == token) {
        Ok(())
    } else {
        Err(TokenValidationError::InvalidBearerToken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_good_header() {
        let tokens = vec!["secret".to_string()];
        assert_eq!(
            validate_bearer_header(Some("Bearer secret"), &tokens),
            Ok(())
        );
    }

    #[test]
    fn rejects_missing_header() {
        let tokens = vec!["secret".to_string()];
        assert_eq!(
            validate_bearer_header(None, &tokens),
            Err(TokenValidationError::MissingAuthorizationHeader)
        );
    }

    #[test]
    fn rejects_non_bearer_scheme() {
        assert_eq!(
            extract_bearer_token("Token abc"),
            Err(TokenValidationError::InvalidAuthorizationScheme)
        );
    }

    #[test]
    fn rejects_invalid_token() {
        let tokens = vec!["secret".to_string()];
        assert_eq!(
            validate_bearer_token("wrong", &tokens),
            Err(TokenValidationError::InvalidBearerToken)
        );
    }
}
