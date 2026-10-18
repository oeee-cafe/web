//! Commenting on a post.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::comment::{
    build_comment_thread_tree, create_comment, find_users_by_login_names, CommentDraft,
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
use minijinja::value::Serde;
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
    /// Empty for a guest, who has no account to name.
    pub login_name: String,
    /// The account's display name, or the name the guest gave.
    pub display_name: String,
    pub is_guest: bool,
}

/// The most people one comment notifies by mentioning them. A comment can
/// name more; past this they are linked and not rung.
pub const MAX_MENTIONS_NOTIFIED: usize = 10;

/// What a comment's mentions are notified about, and who is not to be.
pub struct MentionContext<'a> {
    pub commenter: Uuid,
    pub commenter_actor: Uuid,
    pub post_id: Uuid,
    pub comment_id: Uuid,
    /// The post's community, whose members alone hear about a private one.
    pub community: Option<&'a crate::models::community::Community>,
    /// Who the comment has notified already, as the post's author or the
    /// parent comment's.
    pub already: &'a [Uuid],
}

/// Notifies the accounts `content` mentions, under `context`'s rules, and
/// returns each notification made with its recipient, for its push.
pub async fn notify_mentioned(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    content: &str,
    context: MentionContext<'_>,
) -> Result<Vec<(Uuid, Uuid)>, AppError> {
    let mentioned = crate::mention::login_names(content);
    if mentioned.is_empty() {
        return Ok(Vec::new());
    }
    let mut accounts = find_users_by_login_names(tx, &mentioned).await?;
    // In the order they were written, so the cap keeps the first.
    accounts.sort_by_key(|(_, login_name)| {
        mentioned
            .iter()
            .position(|name| name == login_name)
            .unwrap_or(usize::MAX)
    });
    let mut made = Vec::new();
    for (recipient, _) in accounts {
        if made.len() == MAX_MENTIONS_NOTIFIED {
            break;
        }
        if recipient == context.commenter || context.already.contains(&recipient) {
            continue;
        }
        if let Some(community) = context.community
            && community.visibility == crate::models::community::CommunityVisibility::Private
            && !is_user_member(tx, recipient, community.id).await?
        {
            continue;
        }
        let notification = create_notification(
            tx,
            CreateNotificationParams {
                recipient_id: recipient,
                actor_id: context.commenter_actor,
                notification_type: NotificationType::Mention,
                post_id: Some(context.post_id),
                comment_id: Some(context.comment_id),
                reaction_iri: None,
                guestbook_entry_id: None,
            },
        )
        .await?;
        made.push((notification.id, recipient));
    }
    Ok(made)
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
            if let Some(parent_actor_data) = parent_actor
                && let Some(parent_user_id) = parent_actor_data.user_id
                && parent_user_id != user_id
                && let Ok(notification) = create_notification(
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
    } else {
        // Create notification for the post author (don't notify if commenting on own post)
        // Only send this if it's a top-level comment (no parent)
        if let Some(post_author_id) = post_author_id
            && post_author_id != user_id
            && let Ok(notification) = create_notification(
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

    // Everyone the comment mentions (crate::mention), but the commenter and
    // anyone it has just notified as the post's author or the comment's
    // parent's: one notification each, for the first MAX_MENTIONS_NOTIFIED.
    let notified = notify_mentioned(
        &mut tx,
        comment.content.as_deref().unwrap_or(""),
        MentionContext {
            commenter: user_id,
            commenter_actor: actor.id,
            post_id,
            comment_id: comment.id,
            community: post_community.as_ref(),
            already: &notification_info
                .iter()
                .map(|(_, recipient)| *recipient)
                .collect::<Vec<_>>(),
        },
    )
    .await?;
    notification_info.extend(notified);

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
                comments => Serde(comments),
                current_user => Serde(auth_session.user),
                ftl_lang
            },
        )
        .await?;
    Ok(Html(rendered).into_response())
}

/// How many people the `@` autocomplete lists at once.
const MENTION_SUGGESTIONS: i64 = 8;

#[derive(Deserialize)]
pub struct MentionQuery {
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
pub struct MentionSuggestion {
    login_name: String,
    display_name: String,
}

/// GET /posts/{post_id}/mentions?q= -- who the `@` being typed under a post
/// could be (post_view.jinja's comment box): the people models::people
/// finds, with the post's author and commenters among those nearby when
/// the reader can see the post.
pub async fn mention_suggestions(
    auth_session: AuthSession,
    State(state): State<AppState>,
    axum::extract::Path(post_id): axum::extract::Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<MentionQuery>,
) -> Result<impl IntoResponse, AppError> {
    let reader = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    // No longer than a login name, `@` and all.
    let q: String = query.q.trim().chars().take(51).collect();
    let mut tx = state.db_pool.begin().await?;
    let post = if reader_sees_post(&mut tx, reader, post_id).await? {
        Some(post_id)
    } else {
        None
    };
    let people = crate::models::people::find_people(
        &mut tx,
        &crate::models::people::PeopleQuery {
            reader: Some(reader),
            q: &q,
            post,
            limit: MENTION_SUGGESTIONS,
        },
    )
    .await?;
    tx.commit().await?;
    let suggestions: Vec<MentionSuggestion> = people
        .into_iter()
        .map(|person| MentionSuggestion {
            login_name: person.login_name.to_string(),
            display_name: person.display_name,
        })
        .collect();
    Ok(axum::Json(suggestions))
}

/// Whether `reader` may see the post: it is there, and not in a private
/// community they are not in -- the rule commenting on it keeps.
async fn reader_sees_post(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    reader: Uuid,
    post_id: Uuid,
) -> Result<bool, AppError> {
    let Some(post) = find_post_by_id(tx, post_id).await? else {
        return Ok(false);
    };
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());
    let Some(community_id) = community_id else {
        return Ok(true);
    };
    let Some(community) = find_community_by_id(tx, community_id).await? else {
        return Ok(true);
    };
    Ok(
        community.visibility != crate::models::community::CommunityVisibility::Private
            || get_user_role_in_community(tx, reader, community.id)
                .await?
                .is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::people::fixtures::{comment, person, post, tag, tx};

    async fn mentioned(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        comment_id: Uuid,
    ) -> Vec<Uuid> {
        sqlx::query_scalar!(
            "SELECT recipient_id FROM notifications
             WHERE comment_id = $1 AND notification_type = 'mention'",
            comment_id
        )
        .fetch_all(&mut **tx)
        .await
        .unwrap()
    }

    /// Each account a comment mentions hears of it once -- not the
    /// commenter, not whoever the comment has notified already, not a name
    /// nobody has or an address.
    #[tokio::test]
    async fn a_mention_notifies_each_account_once() {
        let Some(mut tx) = tx().await else { return };
        let t = tag();
        let (writer, writer_actor) = person(&mut tx, &format!("w{t}"), "Writer").await;
        let (author, _) = person(&mut tx, &format!("a{t}"), "Author").await;
        let (friend, _) = person(&mut tx, &format!("f{t}"), "Friend").await;
        person(&mut tx, &format!("o{t}"), "Other").await;
        let (gone, _) = person(&mut tx, &format!("g{t}"), "Gone").await;
        sqlx::query!("UPDATE users SET deleted_at = now() WHERE id = $1", gone)
            .execute(&mut *tx)
            .await
            .unwrap();
        let drawing = post(&mut tx, author).await;
        let content = format!(
            "@a{t} @f{t} and @f{t} again, @w{t}, @g{t}, @nobody{t}, mail@o{t}.example, @o{t}@elsewhere.example"
        );
        let id = comment(&mut tx, drawing, writer_actor, &content).await;

        let made = notify_mentioned(
            &mut tx,
            &content,
            MentionContext {
                commenter: writer,
                commenter_actor: writer_actor,
                post_id: drawing,
                comment_id: id,
                community: None,
                already: &[author],
            },
        )
        .await
        .unwrap();
        assert_eq!(made.iter().map(|(_, to)| *to).collect::<Vec<_>>(), [friend]);
        assert_eq!(mentioned(&mut tx, id).await, [friend]);
    }

    /// A comment rings at most MAX_MENTIONS_NOTIFIED people, the first it
    /// names; the commenter naming themselves does not use one up.
    #[tokio::test]
    async fn a_comment_rings_only_so_many() {
        let Some(mut tx) = tx().await else { return };
        let t = tag();
        let (writer, writer_actor) = person(&mut tx, &format!("w{t}"), "Writer").await;
        let drawing = post(&mut tx, writer).await;
        let mut people = Vec::new();
        let mut content = format!("@w{t}");
        for i in 0..MAX_MENTIONS_NOTIFIED + 2 {
            let name = format!("m{t}{i:02}");
            people.push(person(&mut tx, &name, "Many").await.0);
            content.push_str(&format!(" @{name}"));
        }
        let id = comment(&mut tx, drawing, writer_actor, &content).await;
        let made = notify_mentioned(
            &mut tx,
            &content,
            MentionContext {
                commenter: writer,
                commenter_actor: writer_actor,
                post_id: drawing,
                comment_id: id,
                community: None,
                already: &[],
            },
        )
        .await
        .unwrap();
        assert_eq!(
            made.iter().map(|(_, to)| *to).collect::<Vec<_>>(),
            people[..MAX_MENTIONS_NOTIFIED]
        );
    }
}
