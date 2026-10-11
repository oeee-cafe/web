//! The desktop app's Discord account: trading its tokens, for an app that
//! may not hold the secret (`crate::discord::AppGrant`).
//!
//! Asked by the app itself, from outside any page -- no cookie, no Origin --
//! so nothing here reads a session or answers differently for anyone: what
//! it trades is whatever Discord would trade for the code or token sent,
//! with the secret added, and only a code given to the app's own redirect.
//! It keeps nothing.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::app_error::AppError;
use crate::discord::{self, AppGrant};
use crate::web::state::AppState;

/// Tokens for the app's grant, or 400 when Discord will not give them; 404
/// when the site has no `[discord]` to trade with.
pub async fn app_token(
    State(state): State<AppState>,
    Json(grant): Json<AppGrant>,
) -> Result<Response, AppError> {
    let config = state
        .config
        .discord
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Discord".to_string()))?;
    match discord::app_tokens(config, &grant).await? {
        Some(tokens) => Ok(Json(tokens).into_response()),
        None => Err(AppError::BadRequest(
            "Discord would not trade that".to_string(),
        )),
    }
}

#[derive(Deserialize)]
pub struct Revoke {
    token: String,
}

/// Hands a token back to Discord. Always 204: the app forgets the token
/// whatever Discord says.
pub async fn app_revoke(
    State(state): State<AppState>,
    Json(revoke): Json<Revoke>,
) -> Result<Response, AppError> {
    let config = state
        .config
        .discord
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Discord".to_string()))?;
    if let Err(error) = discord::revoke(config, &revoke.token).await {
        tracing::warn!("Discord could not be asked to revoke the app's token: {error:#}");
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
