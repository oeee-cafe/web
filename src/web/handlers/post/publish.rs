//! Publishing a finished drawing, and the drafts waiting to be.

use crate::app_error::AppError;
use crate::models::achievement::award_achievements;
use crate::models::actor::Actor;
use crate::models::community::{find_community_by_id, is_user_member};
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::{find_draft_posts_by_author_id, find_post_by_id, publish_post};
use crate::models::tag::set_post_tags;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, response::Html, Form};
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use super::federation::{send_post_to_community_followers, send_post_to_followers};
use super::get_community_slug_url;

pub async fn post_publish_form(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;

    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    let published_at = post
        .get("published_at")
        .ok_or_else(|| AppError::BadRequest("Missing published_at".to_string()))?
        .clone();
    if published_at.is_some() {
        return Ok(Redirect::to(&format!("/posts/{}", id)).into_response());
    }

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    let link = if let Some(cid) = community_id {
        Some(get_community_slug_url(&mut tx, cid).await?)
    } else {
        None
    };

    let rendered = state
        .render_page(
            "post_form.jinja",
            common_ctx,
            context! {
                post_id => id,
                link,
                post => {
                    post
                },
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct PostPublishForm {
    post_id: String,
    title: String,
    content: String,
    is_sensitive: Option<String>,
    allow_relay: Option<String>,
    allow_replay: Option<String>,
    tags: Option<String>,
}

pub async fn post_publish(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Form(form): Form<PostPublishForm>,
) -> Result<impl IntoResponse, AppError> {
    let post_id = Uuid::parse_str(&form.post_id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_id).await?;
    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let author_id = Uuid::parse_str(
        post.get("author_id")
            .and_then(|v| v.as_ref())
            .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?,
    )?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    if author_id != user_id {
        return Err(AppError::Forbidden);
    }

    let is_sensitive = form.is_sensitive == Some("on".to_string());
    let allow_relay = form.allow_relay == Some("on".to_string());
    let allow_replay = form.allow_replay == Some("on".to_string());

    // Parse community_id if present, otherwise None for personal posts
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    // Determine redirect URL based on whether post has community
    let redirect_url = if let Some(cid) = community_id {
        get_community_slug_url(&mut tx, cid).await?
    } else {
        // Personal post - redirect to user's profile
        let user = crate::models::user::find_user_by_id(&mut tx, user_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("User not found"))?;
        format!("/@{}", user.login_name)
    };

    let _ = publish_post(
        &mut tx,
        post_id,
        form.title.clone(),
        form.content.clone(),
        is_sensitive,
        allow_relay,
        allow_replay,
    )
    .await;

    // Not `let _ =`: a tag that fails to store aborts the transaction this
    // publish is running in, so swallowing the error only moved the failure to
    // whichever query ran next and made it unattributable.
    set_post_tags(&mut tx, post_id, form.tags.as_deref()).await?;

    // A first drawing, or a first relay.
    award_achievements(&mut tx, user_id).await?;

    // Find the actor for this user to send ActivityPub activities
    let actor = Actor::find_by_user_id(&mut tx, user_id).await?;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Check if this is a reply post and notify the parent post author
    if let Some(parent_post_id_str) = post.get("parent_post_id").and_then(|id| id.as_ref())
        && let Ok(parent_post_id) = Uuid::parse_str(parent_post_id_str)
    {
        let parent_post = find_post_by_id(&mut tx, parent_post_id).await?;
        let parent_author_id = parent_post
            .as_ref()
            .and_then(|p| p.get("author_id"))
            .and_then(|id| id.as_ref())
            .and_then(|id| Uuid::parse_str(id).ok());

        if let (Some(actor), Some(parent_author_id)) = (&actor, parent_author_id) {
            // Don't notify if replying to own post
            if parent_author_id != user_id {
                // For private communities, only notify if parent author is still a member
                // For personal posts (no community), always notify
                let should_notify = if let Some(cid) = community_id {
                    let community = find_community_by_id(&mut tx, cid).await?;
                    if let Some(community) = community {
                        if community.visibility
                            == crate::models::community::CommunityVisibility::Private
                        {
                            // Check if parent author is still a member
                            is_user_member(&mut tx, parent_author_id, community.id)
                                .await
                                .unwrap_or(false)
                        } else {
                            // Public or unlisted community - always notify
                            true
                        }
                    } else {
                        // No community info - notify anyway
                        true
                    }
                } else {
                    // Personal post - always notify
                    true
                };

                if should_notify
                    && let Ok(notification) = create_notification(
                        &mut tx,
                        CreateNotificationParams {
                            recipient_id: parent_author_id,
                            actor_id: actor.id,
                            notification_type: NotificationType::PostReply,
                            post_id: Some(post_id),
                            comment_id: None,
                            reaction_iri: None,
                            guestbook_entry_id: None,
                        },
                    )
                    .await
                {
                    notification_info.push((notification.id, parent_author_id));
                }
            }
        }
    }

    // Notify community participants for new posts in unlisted or private communities
    // Only notify for top-level posts (not replies) and only if post has a community
    let is_reply = post
        .get("parent_post_id")
        .and_then(|id| id.as_ref())
        .is_some();

    if let Some(cid) = community_id
        && !is_reply
        && let Some(ref actor) = actor
    {
        let community = find_community_by_id(&mut tx, cid).await?;

        if let Some(ref community) = community {
            // Only notify for unlisted or private communities
            let should_notify_community = matches!(
                community.visibility,
                crate::models::community::CommunityVisibility::Unlisted
                    | crate::models::community::CommunityVisibility::Private
            );

            if should_notify_community {
                // Get community participants based on visibility
                let participant_ids: Vec<Uuid> = if community.visibility
                    == crate::models::community::CommunityVisibility::Private
                {
                    // For private communities, get all members
                    use crate::models::community::get_community_members;
                    let members = get_community_members(&mut tx, cid).await?;
                    members.into_iter().map(|m| m.user_id).collect()
                } else {
                    // For unlisted communities, get all users who have posted
                    let participants = sqlx::query!(
                        r#"
                                SELECT DISTINCT author_id
                                FROM posts
                                WHERE community_id = $1
                                    AND published_at IS NOT NULL
                                    AND deleted_at IS NULL
                                    AND author_id != $2
                                "#,
                        cid,
                        user_id
                    )
                    .fetch_all(&mut *tx)
                    .await?;
                    participants.into_iter().map(|p| p.author_id).collect()
                };

                // Create notifications for each participant (excluding the post author)
                for participant_id in participant_ids {
                    if participant_id != user_id
                        && let Ok(notification) = create_notification(
                            &mut tx,
                            CreateNotificationParams {
                                recipient_id: participant_id,
                                actor_id: actor.id,
                                notification_type: NotificationType::CommunityPost,
                                post_id: Some(post_id),
                                comment_id: None,
                                reaction_iri: None,
                                guestbook_entry_id: None,
                            },
                        )
                        .await
                    {
                        notification_info.push((notification.id, participant_id));
                    }
                }
            }
        }
    }

    // Determine if we should federate based on post type
    // For personal posts (no community), always federate to user's followers
    // For community posts, only federate if not private
    let (should_federate, community) = if let Some(cid) = community_id {
        let community = find_community_by_id(&mut tx, cid).await?;
        let should_federate = community
            .as_ref()
            .map(|c| c.visibility != crate::models::community::CommunityVisibility::Private)
            .unwrap_or(false);
        (should_federate, community)
    } else {
        // Personal post - always federate to user's followers
        (true, None)
    };

    let _ = tx.commit().await;

    // A new drawing, for the pages showing where it went. Not for a reply,
    // which no feed shows, and not in a private community, whose id is not
    // to be told to every page on the site.
    use crate::models::community::CommunityVisibility;
    let visibility = community.as_ref().map(|c| c.visibility);
    if !is_reply && visibility != Some(CommunityVisibility::Private) {
        state.live.publish(crate::live::LiveEvent::Post {
            community_id,
            recent: !is_sensitive && matches!(visibility, None | Some(CommunityVisibility::Public)),
            by: user_id,
        });
    }

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

                // Get the full notification with actor details
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

    // Send ActivityPub Create activity to followers if actor exists
    // For personal posts: send to user's followers only
    // For community posts: send to both user's and community's followers (if not private)
    if let Some(actor) = actor {
        if should_federate {
            // Send to user's followers first and get the Note object
            match send_post_to_followers(
                &actor,
                post_id,
                form.title.clone(),
                form.content.clone(),
                &state,
            )
            .await
            {
                Ok(note) => {
                    // For community posts, also send to community's followers
                    if let Some(cid) = community_id {
                        if let Err(e) =
                            send_post_to_community_followers(&actor, cid, &note, &state).await
                        {
                            tracing::error!(
                                "Failed to send post to community's ActivityPub followers: {:?}",
                                e
                            );
                            // Don't fail the entire operation if ActivityPub sending fails
                        }
                    } else {
                        tracing::info!("Personal post - sent to user's followers only");
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to send post to user's ActivityPub followers: {:?}",
                        e
                    );
                    // Don't fail the entire operation if ActivityPub sending fails
                }
            }
        } else {
            tracing::info!(
                "Skipping ActivityPub federation for private community post (visibility: {:?})",
                community.as_ref().map(|c| &c.visibility)
            );
        }
    } else {
        tracing::warn!("No actor found for user {}, skipping ActivityPub", user_id);
    }

    Ok(Redirect::to(&redirect_url).into_response())
}

pub async fn draft_posts(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    // A guest has no drafts here, only the ones kept on their device, which
    // the page lists itself.
    let posts = match auth_session.user.as_ref() {
        Some(user) => find_draft_posts_by_author_id(&mut tx, user.id).await?,
        None => Vec::new(),
    };

    tx.commit().await?;

    let rendered = state
        .render_page(
            "draft_posts.jinja",
            common_ctx,
            context! {
                posts => posts,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}
