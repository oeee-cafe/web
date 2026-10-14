//! Giving a post a pickle (`models::pickle`).

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::community::{
    find_community_by_id, get_user_role_in_community, CommunityVisibility,
};
use crate::models::notification::{
    create_notification, push_later, CreateNotificationParams, NotificationType,
};
use crate::models::pickle::{give_weekly, state_for};
use crate::models::post::find_post_by_id;
use crate::models::supporter;
use crate::models::user::AuthSession;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::response::Html;
use minijinja::context;
use minijinja::value::Serde;
use uuid::Uuid;

/// Gives the post this week's pickle, and answers with the post page's
/// pickle chip (`post_pickles.jinja`) as the giver now sees it, for htmx to
/// swap in. Having given this week's already, or having given this post one
/// before, leaves everything as it was and answers the same way, as a second
/// press of a reaction does; the chip says which.
///
/// Only a supporter has a weekly pickle, and nobody may give one to their
/// own post.
pub async fn give_pickle(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(post_id): Path<Uuid>,
) -> Result<Html<String>, AppError> {
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let mut tx = state.db_pool.begin().await?;

    let post = find_post_by_id(&mut tx, post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;
    let field = |name: &str| post.get(name).and_then(|v| v.as_deref());
    let author_id = field("author_id").and_then(|s| Uuid::parse_str(s).ok());

    // A post that is not published yet is nobody else's to see.
    if field("published_at").is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    if let Some(community_id) = field("community_id").and_then(|s| Uuid::parse_str(s).ok())
        && let Some(community) = find_community_by_id(&mut tx, community_id).await?
        && community.visibility == CommunityVisibility::Private
        && get_user_role_in_community(&mut tx, user_id, community.id)
            .await?
            .is_none()
    {
        return Err(AppError::Forbidden);
    }
    if author_id == Some(user_id) {
        return Err(AppError::Forbidden);
    }
    if supporter::mark_for(&mut tx, user_id).await?.is_none() {
        return Err(AppError::Forbidden);
    }

    let given = give_weekly(&mut tx, user_id, post_id).await?;

    // The artist hears of it, by name: a pickle is one person's one for the
    // week, so unlike a reaction it is never folded in with others.
    let mut notifications = Vec::new();
    if given
        && let Some(author_id) = author_id
        && let Some(actor) = Actor::find_by_user_id(&mut tx, user_id).await?
    {
        let notification = create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id: author_id,
                actor_id: actor.id,
                notification_type: NotificationType::Pickle,
                post_id: Some(post_id),
                comment_id: None,
                reaction_iri: None,
                guestbook_entry_id: None,
            },
        )
        .await?;
        notifications.push((notification.id, author_id));
    }

    let pickles = state_for(&mut tx, post_id, Some(user_id), true).await?;
    tx.commit().await?;
    push_later(&state.push_service, &state.db_pool, notifications);

    let rendered = state
        .render(
            "post_pickles.jinja",
            context! {
                current_user => Serde(auth_session.user),
                pickles => Serde(pickles),
                post_id => post_id.to_string(),
                post_author_id => author_id.map(|id| id.to_string()),
                ftl_lang,
            },
        )
        .await?;
    Ok(Html(rendered))
}
