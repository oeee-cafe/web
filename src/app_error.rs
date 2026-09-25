use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use std::fmt;

use crate::web::responses::ErrorResponse;

// Standard error codes
pub mod error_codes {
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const VALIDATION_ERROR: &str = "VALIDATION_ERROR";
    pub const EMAIL_ALREADY_EXISTS: &str = "EMAIL_ALREADY_EXISTS";
    pub const FORBIDDEN: &str = "FORBIDDEN";
    /// A drawing sent for a community this account may not post in. The
    /// painter's drafts page reads it to offer posting it without one.
    pub const COMMUNITY_NOT_ALLOWED: &str = "COMMUNITY_NOT_ALLOWED";
}

/// Check if an error should be filtered from Sentry reporting.
///
/// Federation errors caused by what another server sent — a login page where
/// an actor should be, a tombstone, a bad signature, or nothing at all because
/// the instance is gone (qoto.org, masto.bg) — are not bugs here and
/// arrive at whatever rate the fediverse sends them. Matching on the variant
/// rather than the message matters: each new way a remote can answer with
/// HTML words the serde error differently, and one such wording was 13k
/// events a month (OEEE-CAFE-4B).
fn should_filter_from_sentry(err: &anyhow::Error) -> bool {
    use activitypub_federation::error::Error as FederationError;

    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<FederationError>(),
            Some(
                FederationError::ParseFetchedObject(..)
                    | FederationError::ParseReceivedActivity { .. }
                    | FederationError::ObjectDeleted(..)
                    | FederationError::FetchInvalidContentType(..)
                    | FederationError::FetchWrongId(..)
                    | FederationError::UrlVerificationError(..)
                    | FederationError::ActivitySignatureInvalid
                    | FederationError::ActivityBodyDigestInvalid
                    | FederationError::WebfingerResolveFailed(..)
                    | FederationError::RequestLimit
                    | FederationError::ResponseBodyLimit
                    | FederationError::Reqwest(..)
                    | FederationError::ReqwestMiddleware(..)
            )
        )
    })
}

// Application-specific errors with better context
#[derive(Debug)]
pub enum AppError {
    // Wrap anyhow errors for backward compatibility
    Anyhow(anyhow::Error),

    // Specific error types for better handling
    LocalizationError(String),
    InvalidFormData(String),
    InvalidHash(String),
    InvalidEmail(String),
    InvalidUuid(String),
    InvalidCommunityId(String),
    Unauthorized,
    Forbidden,
    NotFound(String),
    DatabaseError(String),
}

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message, should_capture) = match &self {
            AppError::Anyhow(err) => {
                let message = format!("Something went wrong: {}", err);
                // Capture anyhow errors with full backtrace to Sentry
                if !should_filter_from_sentry(err) {
                    sentry::integrations::anyhow::capture_anyhow(err);
                }

                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    error_codes::INTERNAL_ERROR,
                    message,
                    false,
                )
            }
            AppError::LocalizationError(key) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_codes::INTERNAL_ERROR,
                format!("Missing translation key: {}", key),
                true,
            ),
            AppError::InvalidFormData(msg) => (
                StatusCode::BAD_REQUEST,
                error_codes::VALIDATION_ERROR,
                format!("Invalid form data: {}", msg),
                true,
            ),
            AppError::InvalidHash(msg) => (
                StatusCode::BAD_REQUEST,
                error_codes::VALIDATION_ERROR,
                format!("Invalid hash: {}", msg),
                true,
            ),
            AppError::InvalidEmail(msg) => (
                StatusCode::BAD_REQUEST,
                error_codes::VALIDATION_ERROR,
                format!("Invalid email: {}", msg),
                true,
            ),
            AppError::InvalidUuid(msg) => (
                StatusCode::BAD_REQUEST,
                error_codes::VALIDATION_ERROR,
                format!("Invalid UUID: {}", msg),
                true,
            ),
            AppError::InvalidCommunityId(msg) => (
                StatusCode::BAD_REQUEST,
                error_codes::VALIDATION_ERROR,
                format!("Invalid community ID: {}", msg),
                true,
            ),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                error_codes::UNAUTHORIZED,
                "Unauthorized".to_string(),
                true,
            ),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                error_codes::FORBIDDEN,
                "Forbidden".to_string(),
                true,
            ),
            // A link to something deleted, or a crawler guessing URLs: the
            // caller's business, and not worth a Sentry event apiece.
            AppError::NotFound(resource) => (
                StatusCode::NOT_FOUND,
                error_codes::NOT_FOUND,
                format!("{} not found", resource),
                false,
            ),
            AppError::DatabaseError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_codes::INTERNAL_ERROR,
                format!("Database error: {}", msg),
                true,
            ),
        };

        // Capture non-anyhow errors as messages (no backtrace available since they're just strings)
        if should_capture {
            let sentry_level = match status {
                StatusCode::INTERNAL_SERVER_ERROR => sentry::Level::Error,
                StatusCode::BAD_REQUEST => sentry::Level::Info,
                StatusCode::UNAUTHORIZED => sentry::Level::Info,
                _ => sentry::Level::Warning,
            };
            sentry::capture_message(&message, sentry_level);
        }

        (status, Json(ErrorResponse::new(code, message))).into_response()
    }
}

// Implement Display for AppError
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Anyhow(err) => write!(f, "{}", err),
            AppError::LocalizationError(key) => write!(f, "Missing translation key: {}", key),
            AppError::InvalidFormData(msg) => write!(f, "Invalid form data: {}", msg),
            AppError::InvalidHash(msg) => write!(f, "Invalid hash: {}", msg),
            AppError::InvalidEmail(msg) => write!(f, "Invalid email: {}", msg),
            AppError::InvalidUuid(msg) => write!(f, "Invalid UUID: {}", msg),
            AppError::InvalidCommunityId(msg) => write!(f, "Invalid community ID: {}", msg),
            AppError::Unauthorized => write!(f, "Unauthorized"),
            AppError::Forbidden => write!(f, "Forbidden"),
            AppError::NotFound(resource) => write!(f, "{} not found", resource),
            AppError::DatabaseError(msg) => write!(f, "Database error: {}", msg),
        }
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        AppError::Anyhow(err.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use activitypub_federation::error::Error as FederationError;

    #[test]
    fn a_remote_answering_with_html_is_not_reported() {
        let body = r#"<html><body>You are being <a href="https://social.cleverlibre.org/about">redirected</a>.</body></html>"#;
        let parse = serde_json::from_str::<serde_json::Value>(body).unwrap_err();
        let url = "https://social.cleverlibre.org/".parse().unwrap();
        let AppError::Anyhow(err) = AppError::from(FederationError::ParseFetchedObject(
            parse,
            url,
            body.to_string(),
        )) else {
            unreachable!()
        };
        assert!(should_filter_from_sentry(&err));
        assert!(should_filter_from_sentry(
            &err.context("while fetching an actor")
        ));
    }

    #[test]
    fn our_own_failures_are_still_reported() {
        assert!(!should_filter_from_sentry(&anyhow::anyhow!(
            "Failed to parse object"
        )));
        assert!(!should_filter_from_sentry(
            &FederationError::Other("x".into()).into()
        ));
    }
}
