//! Commenting on a post.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::comment::{
    build_comment_thread_tree, create_comment, extract_mentions, find_users_by_login_names,
    CommentDraft,
};
use crate::models::community::{find_community_by_id, get_user_role_in_community, is_user_member};
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::models::user::AuthSession;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::response::IntoResponse;
use axum::{extract::State, response::Html, Form};
use minijinja::context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateCommentForm {
    pub post_id: String,
    pub parent_comment_id: Option<String>,
    pub content: String,
}

#[derive(Serialize)]
pub struct CollaborativeParticipant {
    pub login_name: String,
    pub display_name: String,
}

pub async fn do_create_comment(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Form(form): Form<CreateCommentForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let post_id = Uuid::parse_str(&form.post_id)
        .map_err(|e| AppError::BadRequest(format!("Invalid UUID: {}", e)))?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Get the post to find the author and check access
    let post = find_post_by_id(&mut tx, post_id).await?;

    let post_community = if let Some(ref post_data) = post {
        // Check if post is in a private/unlisted community and if user has access
        let community_id = post_data
            .get("community_id")
            .and_then(|v| v.as_ref())
            .and_then(|s| Uuid::parse_str(s).ok());

        if let Some(cid) = community_id {
            let community = find_community_by_id(&mut tx, cid).await?;
            if let Some(ref comm) = community {
                // If community is private, check if user is a member
                if comm.visibility == crate::models::community::CommunityVisibility::Private {
                    let user_role = get_user_role_in_community(&mut tx, user_id, comm.id).await?;
                    if user_role.is_none() {
                        // User is not a member of this private community
                        return Err(AppError::Forbidden);
                    }
                }
            }
            community
        } else {
            // Personal post - no community
            None
        }
    } else {
        return Err(AppError::NotFound("Post".to_string()));
    };

    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    // Parse parent_comment_id if provided
    let parent_comment_id = form
        .parent_comment_id
        .as_ref()
        .and_then(|id| Uuid::parse_str(id).ok());

    let comment = create_comment(
        &mut tx,
        CommentDraft {
            actor_id: actor.id,
            post_id,
            parent_comment_id,
            content: form.content,
            content_html: None,
        },
    )
    .await?;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // If this is a reply to another comment, notify the parent comment author
    if let Some(parent_id) = parent_comment_id {
        // Fetch the parent comment to get its author
        let parent_comment = sqlx::query!(
            r#"
            SELECT actor_id
            FROM comments
            WHERE id = $1
            "#,
            parent_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(parent) = parent_comment {
            // Get the user_id from the parent comment's actor
            let parent_actor = sqlx::query!(
                r#"
                SELECT user_id
                FROM actors
                WHERE id = $1
                "#,
                parent.actor_id
            )
            .fetch_optional(&mut *tx)
            .await?;

            // Only notify if the parent comment author is a local user and not the same as current user
            if let Some(parent_actor_data) = parent_actor {
                if let Some(parent_user_id) = parent_actor_data.user_id {
                    if parent_user_id != user_id {
                        if let Ok(notification) = create_notification(
                            &mut tx,
                            CreateNotificationParams {
                                recipient_id: parent_user_id,
                                actor_id: actor.id,
                                notification_type: NotificationType::CommentReply,
                                post_id: Some(post_id),
                                comment_id: Some(comment.id),
                                reaction_iri: None,
                                guestbook_entry_id: None,
                            },
                        )
                        .await
                        {
                            notification_info.push((notification.id, parent_user_id));
                        }
                    }
                }
            }
        }
    } else {
        // Create notification for the post author (don't notify if commenting on own post)
        // Only send this if it's a top-level comment (no parent)
        if let Some(post_author_id) = post_author_id {
            if post_author_id != user_id {
                if let Ok(notification) = create_notification(
                    &mut tx,
                    CreateNotificationParams {
                        recipient_id: post_author_id,
                        actor_id: actor.id,
                        notification_type: NotificationType::Comment,
                        post_id: Some(post_id),
                        comment_id: Some(comment.id),
                        reaction_iri: None,
                        guestbook_entry_id: None,
                    },
                )
                .await
                {
                    notification_info.push((notification.id, post_author_id));
                }
            }
        }
    }

    // Extract @mentions from comment content and create notifications
    let mentioned_login_names = extract_mentions(comment.content.as_deref().unwrap_or(""));
    if !mentioned_login_names.is_empty() {
        let mentioned_users = find_users_by_login_names(&mut tx, &mentioned_login_names).await?;
        for (mentioned_user_id, _login_name) in mentioned_users {
            // Don't notify the commenter themselves
            if mentioned_user_id != user_id {
                // For private communities, only notify if mentioned user is a member
                let should_notify = if let Some(ref community) = post_community {
                    if community.visibility
                        == crate::models::community::CommunityVisibility::Private
                    {
                        // Check if mentioned user is a member
                        is_user_member(&mut tx, mentioned_user_id, community.id)
                            .await
                            .unwrap_or(false)
                    } else {
                        // Public or unlisted community - always notify
                        true
                    }
                } else {
                    // No community info - notify anyway
                    true
                };

                if should_notify {
                    if let Ok(notification) = create_notification(
                        &mut tx,
                        CreateNotificationParams {
                            recipient_id: mentioned_user_id,
                            actor_id: actor.id,
                            notification_type: NotificationType::Mention,
                            post_id: Some(post_id),
                            comment_id: Some(comment.id),
                            reaction_iri: None,
                            guestbook_entry_id: None,
                        },
                    )
                    .await
                    {
                        notification_info.push((notification.id, mentioned_user_id));
                    }
                }
            }
        }
    }

    // At this point post is guaranteed to be Some (would have returned 404 otherwise)
    // post is not used after this point, no need to unwrap

    let comments = build_comment_thread_tree(&mut tx, post_id).await?;
    let _ = tx.commit().await;
    // Everyone else looking at this post fetches its comments again; this
    // page has them in the response.
    state.live.publish(crate::live::LiveEvent::Comments {
        post_id,
        by: Some(user_id),
    });

    // Send push notifications for created notifications
    if !notification_info.is_empty() {
        let push_service = state.push_service.clone();
        let db_pool = state.db_pool.clone();
        tokio::spawn(async move {
            for (notification_id, recipient_id) in notification_info {
                let mut tx = match db_pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to begin transaction for push notification: {:?}",
                            e
                        );
                        continue;
                    }
                };

                if let Ok(Some(notification)) =
                    get_notification_by_id(&mut tx, notification_id, recipient_id).await
                {
                    // The number on the bell, for the icon's badge
                    let badge_count = get_badge_count(&mut tx, recipient_id)
                        .await
                        .ok()
                        .and_then(|count| u32::try_from(count).ok());

                    send_push_for_notification(&push_service, &db_pool, &notification, badge_count)
                        .await;
                }
                let _ = tx.commit().await;
            }
        });
    }

    let rendered = state
        .render(
            "post_comments.jinja",
            context! {
                comments => comments,
                current_user => auth_session.user,
                ftl_lang
            },
        )
        .await?;
    Ok(Html(rendered).into_response())
}
