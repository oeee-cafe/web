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
    pub const INVALID_CREDENTIALS: &str = "INVALID_CREDENTIALS";
    pub const EMAIL_ALREADY_EXISTS: &str = "EMAIL_ALREADY_EXISTS";
    pub const USERNAME_ALREADY_EXISTS: &str = "USERNAME_ALREADY_EXISTS";
    pub const EMAIL_ALREADY_VERIFIED: &str = "EMAIL_ALREADY_VERIFIED";
    pub const INVALID_VERIFICATION_CODE: &str = "INVALID_VERIFICATION_CODE";
    pub const FORBIDDEN: &str = "FORBIDDEN";
    pub const CONFLICT: &str = "CONFLICT";
    /// A drawing sent for a community this account may not post in. The
    /// painter's drafts page reads it to offer posting it without one.
    pub const COMMUNITY_NOT_ALLOWED: &str = "COMMUNITY_NOT_ALLOWED";
}

/// Check if an error should be filtered from Sentry reporting.
/// These are expected federation errors that shouldn't be treated as application errors.
fn should_filter_from_sentry(message: &str) -> bool {
    // Filter out ActivityPub federation errors that are expected
    let federation_error_patterns = [
        // Remote actors that return 404 or invalid JSON
        ("Failed to parse object", "data did not match any variant of untagged enum ActorObject"),
        // Remote objects that have been deleted/tombstoned
        ("Fetched remote object", "which was deleted"),
    ];

    for (pattern1, pattern2) in federation_error_patterns.iter() {
        if message.contains(pattern1) && message.contains(pattern2) {
            return true;
        }
    }

    false
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
                let should_capture = !should_filter_from_sentry(&message);

                // Capture anyhow errors with full backtrace to Sentry
                if should_capture {
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
            AppError::NotFound(resource) => (
                StatusCode::NOT_FOUND,
                error_codes::NOT_FOUND,
                format!("{} not found", resource),
                true,
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
                StatusCode::NOT_FOUND => sentry::Level::Info,
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
