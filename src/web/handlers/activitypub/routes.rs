//! Where a browser that opens one of this site's ActivityPub IDs is sent.
//!
//! What other servers fetch and post is ojak's (src/federation): these
//! routes are reached only by a request that did not ask for ActivityPub,
//! which is a person, who is sent to the page the ID is of.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::post::find_post_by_id;
use crate::web::state::AppState;

/// `/ap/users/{user_id}`, asked for as a page: the profile.
pub async fn activitypub_user_page(
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Ok(user_id) = Uuid::parse_str(&user_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = state.db_pool.begin().await?;
    Ok(match Actor::find_by_user_id(&mut tx, user_id).await? {
        Some(actor) => Redirect::to(&actor.url).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

/// `/ap/communities/{community_id}`, asked for as a page: the community.
pub async fn activitypub_community_page(
    Path(community_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Ok(community_id) = Uuid::parse_str(&community_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = state.db_pool.begin().await?;
    Ok(
        match Actor::find_by_community_id(&mut tx, community_id).await? {
            Some(actor) => Redirect::to(&actor.url).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    )
}

/// `/ap/posts/{post_id}`, asked for as a page: the post.
pub async fn activitypub_post_page(
    Path(post_id): Path<String>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Ok(post_id) = Uuid::parse_str(&post_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = state.db_pool.begin().await?;
    let Some(post) = find_post_by_id(&mut tx, post_id).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(login_name) = post.get("login_name").and_then(|v| v.as_deref()) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let community_slug = post.get("community_slug").and_then(|v| v.as_deref());
    let url = crate::models::post::post_page_url(
        &state.config.domain,
        login_name,
        community_slug,
        post_id,
    );
    Ok(Redirect::to(&url).into_response())
}
