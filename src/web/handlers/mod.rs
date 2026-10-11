use crate::app_error::AppError;
use crate::models::user::{AuthSession, User};
use anyhow;
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{extract::FromRequestParts, http::request::Parts};
use data_encoding::BASE64URL_NOPAD;
use uuid::Uuid;

use super::state::AppState;
use minijinja::context;

pub mod about;
pub mod account;
pub mod activitypub;
pub mod admin;
pub mod auth;
pub mod collaborate;
pub mod collaborate_cleanup;
pub mod community;
pub mod devices;
pub mod discord_app;
pub mod draw;
pub mod events;
pub mod home;
pub mod identity;
pub mod jump;
pub mod notifications;
pub mod password_reset;
pub mod policy;
pub mod post;
pub mod privacy;
pub mod profile;
pub mod report;
pub mod search;
pub mod store;
pub mod supporter;
pub mod tag;
pub mod well_known;

#[cfg(test)]
mod community_page_tests;
#[cfg(test)]
mod social_meta_tests;
#[cfg(test)]
mod template_tests;
#[cfg(test)]
pub(crate) mod test_support;

/// Anything no route matched. `web::error_pages` draws the page for a
/// browser; everybody else gets the JSON.
pub async fn handler_404() -> AppError {
    AppError::NotFound("Page".to_string())
}

/// Liveness/readiness probe. Checks that a connection can actually be taken
/// from the pool and used, so a wedged or exhausted pool fails the check
/// instead of reporting healthy because the process is still running.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db_pool)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::error!("health check failed: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

/// Extractor that admits only site-wide admins.
///
/// Every handler that reads through the normal visibility rules (private
/// communities, drafts, soft-deleted posts) takes this, so the unfiltered
/// queries in `models::admin` are unreachable without it.
pub struct AdminUser(pub User);

impl<S> FromRequestParts<S> for AdminUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_session = AuthSession::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Anyhow(anyhow::anyhow!("Failed to extract auth session")))?;

        let user = auth_session.user.ok_or(AppError::Unauthorized)?;

        if !user.is_admin() {
            return Err(AppError::Forbidden);
        }

        Ok(AdminUser(user))
    }
}

/// Parse ID from URL path, supporting both UUID format and legacy base64 format.
/// Returns either the parsed UUID, a redirect response for legacy URLs, or an error response.
pub enum ParsedId {
    Uuid(Uuid),
    Redirect(axum::response::Redirect),
    InvalidId(axum::response::Response),
}

pub fn parse_id_with_legacy_support(
    id_str: &str,
    base_path: &str,
    state: &crate::web::state::AppState,
) -> Result<ParsedId, AppError> {
    // First try to parse as UUID directly
    if let Ok(uuid) = Uuid::parse_str(id_str) {
        return Ok(ParsedId::Uuid(uuid));
    }

    // If that fails, try to decode as base64 and then parse as UUID
    match BASE64URL_NOPAD.decode(id_str.as_bytes()) {
        Ok(decoded_bytes) => {
            // Try to parse bytes directly as UUID (16 bytes expected)
            if decoded_bytes.len() == 16
                && let Ok(uuid) = Uuid::from_slice(&decoded_bytes)
            {
                // Create redirect to UUID version
                let redirect_url = format!("{}/{}", base_path, uuid);
                return Ok(ParsedId::Redirect(axum::response::Redirect::permanent(
                    &redirect_url,
                )));
            }
        }
        Err(_) => {
            // Not valid base64, continue to error handling
        }
    }

    // If neither UUID nor base64 decoding worked, render custom error page
    // A page with nobody on it, so it is drawn here without a request's
    // database to ask about anyone.
    if let Ok(rendered) = state
        .env
        .render_without_people("invalid_id_error.jinja", context! {})
    {
        let response = axum::response::Html(rendered).into_response();
        return Ok(ParsedId::InvalidId(response));
    }

    // Fallback to generic error if template rendering fails
    Err(AppError::from(anyhow::anyhow!("Invalid ID format")))
}

/// Helper function to safely parse a UUID string
pub fn safe_parse_uuid(s: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(s).map_err(|e| AppError::BadRequest(format!("Invalid UUID {}: {}", s, e)))
}

/// Helper function to safely decode a hex hash string
pub fn safe_decode_hash(s: &str) -> Result<Vec<u8>, AppError> {
    data_encoding::HEXLOWER
        .decode(s.as_bytes())
        .map_err(|e| AppError::BadRequest(format!("Invalid hash {}: {}", s, e)))
}

/// Helper function to safely parse an email address
pub fn safe_parse_email(s: &str) -> Result<lettre::Address, AppError> {
    s.parse()
        .map_err(|e| AppError::BadRequest(format!("Invalid email {}: {}", s, e)))
}
