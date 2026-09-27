//! Posts: viewing, publishing, editing, commenting on and reacting to them.

use crate::app_error::AppError;
use crate::models::community::find_community_by_id;
use crate::models::user::Language;
use crate::web::i18n::{get_bundle, safe_get_message};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use axum_messages::Messages;
use urlencoding;
use uuid::Uuid;

mod federation;
mod view;
pub use view::*;
mod publish;
pub use publish::*;
mod comment;
pub use comment::*;
mod edit;
pub use edit::*;
mod reactions;
pub use reactions::*;

// Helper function to get community @slug URL from UUID
async fn get_community_slug_url(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community_id: uuid::Uuid,
) -> Result<String, AppError> {
    let community = find_community_by_id(tx, community_id).await?;
    if let Some(community) = community {
        Ok(format!("/@{}", community.slug))
    } else {
        Ok(format!("/communities/{}", community_id)) // Fallback to UUID if community not found
    }
}

/// The redirect to a post's canonical page, or to `suffix` beneath it, for a
/// request that named the post under any other handle. `None` when `handle`
/// is already the one `post_page_path` gives.
fn redirect_to_canonical_post(
    handle: &str,
    author_login_name: &str,
    community: Option<&crate::models::community::Community>,
    post_id: Uuid,
    suffix: &str,
) -> Option<axum::response::Response> {
    let community_slug = community.map(|community| community.slug.as_str());
    if handle == community_slug.unwrap_or(author_login_name) {
        return None;
    }
    let path = crate::models::post::post_page_path(author_login_name, community_slug, post_id);
    Some(Redirect::to(&format!("{path}{suffix}")).into_response())
}

/// Helper function to show a flash error message and redirect
fn flash_error_and_redirect(
    headers: &HeaderMap,
    user_preferred_language: Option<Language>,
    messages: Messages,
    message_key: &str,
    redirect_path: &str,
) -> axum::response::Response {
    let accept_language = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .cloned()
        .unwrap_or_else(|| axum::http::HeaderValue::from_static(""));
    let bundle = get_bundle(&accept_language, user_preferred_language);
    let error_message = safe_get_message(&bundle, message_key);
    messages.error(error_message);
    Redirect::to(redirect_path).into_response()
}

/// Helper function to redirect unauthenticated users to login with next parameter
fn redirect_to_login(current_path: &str) -> axum::response::Response {
    let login_url = format!("/login?next={}", urlencoding::encode(current_path));
    Redirect::to(&login_url).into_response()
}
