//! Making, editing and deleting a community.

use crate::app_error::AppError;
use crate::models::actor::create_actor_for_community;
use crate::models::community::{
    create_community, find_community_by_id, find_community_by_slug, slug_conflicts_with_user,
    soft_delete_community_with_activity, update_community_with_activity, CommunityDraft,
    CommunityVisibility,
};
use crate::models::user::AuthSession;
use crate::web::state::AppState;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, Form};
use axum_messages::Messages;
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use crate::web::context::CommonContext;
use crate::web::i18n::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};

use super::page::community_header_context;
use minijinja::value::Serde;

#[derive(Deserialize)]
pub struct CreateCommunityForm {
    name: String,
    slug: String,
    description: String,
    visibility: String,
}

pub async fn do_create_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    messages: Messages,
    Form(form): Form<CreateCommunityForm>,
) -> Result<impl IntoResponse, AppError> {
    if form.name.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Parse visibility from form
    let visibility = match form.visibility.as_str() {
        "public" => CommunityVisibility::Public,
        "unlisted" => CommunityVisibility::Unlisted,
        "private" => CommunityVisibility::Private,
        _ => CommunityVisibility::Public, // Default to public
    };

    // Check if slug conflicts with any user login_name
    if slug_conflicts_with_user(&mut tx, &form.slug).await? {
        let user_preferred_language = auth_session
            .user
            .clone()
            .map(|u| u.preferred_language)
            .unwrap_or_else(|| None);
        let bundle = get_bundle(&accept_language, user_preferred_language);
        let error_message = safe_get_message(&bundle, "community-slug-conflict-error");
        messages.error(error_message);
        return Ok(Redirect::to("/communities/new").into_response());
    }

    let community = create_community(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        CommunityDraft {
            name: form.name,
            slug: form.slug,
            description: form.description,
            visibility,
        },
    )
    .await?;

    // Create actor for the community (only for non-member_only communities)
    if visibility != CommunityVisibility::Private {
        match create_actor_for_community(&mut tx, &community, &state.config, &state.uris).await {
            Ok(_) => {
                let _ = tx.commit().await;
                Ok(Redirect::to(&format!("/@{}", community.slug)).into_response())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                // Check if it's a unique constraint violation (handle conflict)
                if let Some(sqlx::Error::Database(db_err)) = e.downcast_ref::<sqlx::Error>()
                    && db_err.constraint().is_some()
                {
                    let user_preferred_language = auth_session
                        .user
                        .clone()
                        .map(|u| u.preferred_language)
                        .unwrap_or_else(|| None);
                    let bundle = get_bundle(&accept_language, user_preferred_language);
                    let error_message = safe_get_message(&bundle, "community-slug-conflict-error");
                    messages.error(error_message);
                    return Ok(Redirect::to("/communities/new").into_response());
                }
                // For other errors, re-throw
                Err(e.into())
            }
        }
    } else {
        // Member-only community, no actor needed
        let _ = tx.commit().await;
        Ok(Redirect::to(&format!("/communities/@{}", community.slug)).into_response())
    }
}

pub async fn create_community_form(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    messages: Messages,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let rendered = state
        .render_page(
            "create_community.jinja",
            common_ctx,
            context! {
                messages => Serde(messages.into_iter().collect::<Vec<_>>()),
            },
        )
        .await?;

    Ok(Html(rendered))
}

pub async fn hx_edit_community(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::BadRequest("Invalid slug format".to_string()))?
            .to_string();
        find_community_by_slug(&mut tx, slug).await?
    } else {
        // Handle UUID format - redirect to @slug
        let community_uuid = Uuid::parse_str(&id)?;
        let community = find_community_by_id(&mut tx, community_uuid).await?;
        if let Some(community) = &community {
            // Redirect UUID to @slug format
            return Ok(
                Redirect::to(&format!("/communities/@{}/edit", community.slug)).into_response(),
            );
        } else {
            None
        }
    };

    if community.is_none() {
        return Err(AppError::NotFound("Community".to_string()));
    }

    if community
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Community".to_string()))?
        .owner_id
        != auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id
    {
        return Err(AppError::Forbidden);
    }

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let rendered = state
        .render(
            "community_edit.jinja",
            context! {
                current_user => Serde(auth_session.user),
                community => Serde(community),
                community_id => id,
                domain => state.config.domain.clone(),
                unread_notification_count => common_ctx.unread_notification_count,
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

pub async fn hx_do_edit_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<CreateCommunityForm>,
) -> Result<impl IntoResponse, AppError> {
    if form.name.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let (community_uuid, original_slug) = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::BadRequest("Invalid slug format".to_string()))?
            .to_string();
        let community = find_community_by_slug(&mut tx, slug.clone()).await?;
        if let Some(community) = community {
            (community.id, community.slug)
        } else {
            return Err(AppError::NotFound("Community".to_string()));
        }
    } else {
        // Handle UUID format - redirect to @slug
        let uuid = Uuid::parse_str(&id)?;
        let community = find_community_by_id(&mut tx, uuid).await?;
        if let Some(_community) = &community {
            // Redirect UUID to @slug format for PUT request
            return Ok(StatusCode::PERMANENT_REDIRECT.into_response());
        } else {
            return Err(AppError::NotFound("Community".to_string()));
        }
    };

    // Update the community (with ActivityPub Update activity)
    // Parse visibility from form
    let visibility = match form.visibility.as_str() {
        "public" => CommunityVisibility::Public,
        "unlisted" => CommunityVisibility::Unlisted,
        "private" => CommunityVisibility::Private,
        _ => CommunityVisibility::Public, // Default to public
    };

    let community_draft = CommunityDraft {
        name: form.name.clone(),
        slug: form.slug.clone(),
        description: form.description.clone(),
        visibility,
    };

    match update_community_with_activity(
        &mut tx,
        community_uuid,
        community_draft,
        &state.config,
        Some(&state),
    )
    .await
    {
        Ok(updated_community) => {
            // Success - commit transaction
            let _ = tx.commit().await;

            // Check if slug changed - if so, redirect entire page to new URL
            if form.slug != original_slug {
                // Use HTMX redirect to navigate to new slug URL
                Ok(([(
                    "HX-Redirect",
                    format!("/communities/@{}", form.slug).as_str(),
                )],)
                    .into_response())
            } else {
                // Slug didn't change - return updated content block
                let template = "community.jinja";
                let user_preferred_language = auth_session
                    .user
                    .clone()
                    .map(|u| u.preferred_language)
                    .unwrap_or_else(|| None);
                let bundle = get_bundle(&accept_language, user_preferred_language);
                let ftl_lang = bundle
                    .locales
                    .first()
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "en".to_string())
                    .to_string();
                let mut header_tx = state.db_pool.begin().await?;
                let header = community_header_context(&mut header_tx, &updated_community).await?;
                header_tx.commit().await?;
                let rendered = state
                    .render_block(
                        template,
                        "community_edit_block",
                        context! {
                            current_user => Serde(auth_session.user),
                            header => header,
                            community => Serde(&updated_community),
                            community_id => updated_community.id.to_string(),
                            domain => state.config.domain.clone(),
                            ftl_lang
                        },
                    )
                    .await?;

                Ok(Html(rendered).into_response())
            }
        }
        Err(e) => {
            // Error - rollback transaction and return edit form with error
            let _ = tx.rollback().await;

            // Check if it's a constraint violation (slug conflict)
            let error_message =
                if let Some(sqlx::Error::Database(db_err)) = e.downcast_ref::<sqlx::Error>() {
                    if db_err.constraint().is_some() {
                        let user_preferred_language = auth_session
                            .user
                            .clone()
                            .map(|u| u.preferred_language)
                            .unwrap_or_else(|| None);
                        let bundle = get_bundle(&accept_language, user_preferred_language);
                        Some(safe_get_message(&bundle, "community-slug-conflict-error"))
                    } else {
                        None
                    }
                } else {
                    None
                };

            // Get current community data to show in the form
            let mut tx = db.begin().await?;
            let current_community = find_community_by_id(&mut tx, community_uuid).await?;

            let user_preferred_language = auth_session
                .user
                .clone()
                .map(|u| u.preferred_language)
                .unwrap_or_else(|| None);
            let bundle = get_bundle(&accept_language, user_preferred_language);
            let ftl_lang = bundle
                .locales
                .first()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "en".to_string())
                .to_string();
            let rendered = state
                .render(
                    "community_edit.jinja",
                    context! {
                        current_user => Serde(auth_session.user),
                        community => Serde(current_community),
                        community_id => id,
                        domain => state.config.domain.clone(),
                        error_message => error_message,
                        ftl_lang
                    },
                )
                .await?;

            Ok(Html(rendered).into_response())
        }
    }
}

/// DELETE handler for web interface (HTMX)
pub async fn hx_delete_community(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Verify user is authenticated
    let user = match &auth_session.user {
        Some(u) => u,
        None => return Err(AppError::Unauthorized),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Attempt to delete the community
    let falls =
        soft_delete_community_with_activity(&mut tx, &slug, user.id, &state.config, Some(&state))
            .await?;

    tx.commit().await?;
    state.push_service.badges_fell(falls);

    // Redirect to communities list
    Ok(([("HX-Redirect", "/communities")],).into_response())
}
