use anyhow::Result;
use chrono::{DateTime, Utc};
use fluent::bundle::FluentBundle;
use fluent::{FluentArgs, FluentResource};
use intl_memoizer::concurrent::IntlLangMemoizer;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction, Type};
use uuid::Uuid;

use crate::locale::LOCALES;
use crate::models::user::Language;

#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
#[sqlx(type_name = "notification_type", rename_all = "lowercase")]
pub enum NotificationType {
    Comment,
    Reaction,
    Follow,
    #[sqlx(rename = "guestbook_entry")]
    GuestbookEntry,
    #[sqlx(rename = "guestbook_reply")]
    GuestbookReply,
    Mention,
    #[sqlx(rename = "post_reply")]
    PostReply,
    #[sqlx(rename = "comment_reply")]
    CommentReply,
    #[sqlx(rename = "community_post")]
    CommunityPost,
}

#[derive(Clone, Debug, Serialize)]
pub struct Notification {
    pub id: Uuid,
    pub recipient_id: Uuid,
    pub actor_id: Uuid,
    pub notification_type: NotificationType,
    pub post_id: Option<Uuid>,
    pub comment_id: Option<Uuid>,
    pub reaction_iri: Option<String>,
    pub guestbook_entry_id: Option<Uuid>,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationWithActor {
    pub id: Uuid,
    pub recipient_id: Uuid,
    pub actor_id: Uuid,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_login_name: Option<String>,
    pub notification_type: NotificationType,
    pub post_id: Option<Uuid>,
    pub comment_id: Option<Uuid>,
    pub reaction_iri: Option<String>,
    pub reaction_emoji: Option<String>,
    pub guestbook_entry_id: Option<Uuid>,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    // Additional context data
    pub post_title: Option<String>,
    pub post_author_login_name: Option<String>,
    pub post_image_filename: Option<String>,
    pub post_image_width: Option<i32>,
    pub post_image_height: Option<i32>,
    pub comment_content: Option<String>,
    pub comment_content_html: Option<String>,
    pub guestbook_content: Option<String>,
    /// How many distinct people this row stands for. 1 for everything that is
    /// not a collapsed reaction group; the row then reads "X and N others".
    pub actor_count: i64,
}

pub struct CreateNotificationParams {
    pub recipient_id: Uuid,
    pub actor_id: Uuid,
    pub notification_type: NotificationType,
    pub post_id: Option<Uuid>,
    pub comment_id: Option<Uuid>,
    pub reaction_iri: Option<String>,
    pub guestbook_entry_id: Option<Uuid>,
}

/// Create a new notification
pub async fn create_notification(
    tx: &mut Transaction<'_, Postgres>,
    params: CreateNotificationParams,
) -> Result<Notification> {
    // Don't create notification if actor is notifying themselves
    // For federated actors (user_id is None), they can't be self-notifications
    if let Ok(actor_user_id) = get_user_id_from_actor(tx, params.actor_id).await {
        if params.recipient_id == actor_user_id {
            return Err(anyhow::anyhow!("Cannot notify self"));
        }
    }

    let notification = sqlx::query_as!(
        Notification,
        r#"
        INSERT INTO notifications (
            recipient_id, actor_id, notification_type,
            post_id, comment_id, reaction_iri, guestbook_entry_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING
            id, recipient_id, actor_id,
            notification_type as "notification_type: NotificationType",
            post_id, comment_id, reaction_iri, guestbook_entry_id,
            read_at, created_at
        "#,
        params.recipient_id,
        params.actor_id,
        params.notification_type as NotificationType,
        params.post_id,
        params.comment_id,
        params.reaction_iri,
        params.guestbook_entry_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(notification)
}

/// Helper function to get user_id from actor_id (returns None if actor has no associated user)
async fn get_user_id_from_actor(
    tx: &mut Transaction<'_, Postgres>,
    actor_id: Uuid,
) -> Result<Uuid> {
    let result = sqlx::query!(
        r#"
        SELECT user_id FROM actors WHERE id = $1
        "#,
        actor_id
    )
    .fetch_one(&mut **tx)
    .await?;

    result
        .user_id
        .ok_or_else(|| anyhow::anyhow!("Actor has no associated user"))
}

/// List notifications for a user, newest first, with every reaction on one post
/// collapsed into a single row.
///
/// Reactions are 55% of all notifications here and they arrive in clumps: one
/// drawing picked up sixteen of them, and 181 of the 234 in the table share a
/// post with at least one other. Collapsing them cuts the list by 41% overall.
///
/// The grouping happens in SQL rather than over an already-fetched page so that
/// `limit`/`offset` count *rows the reader sees*. Grouping afterwards would make
/// page size vary with content and let a group straddle a page boundary, which
/// is exactly the kind of thing that makes an infinite scroll skip or repeat.
pub async fn list_notifications(
    tx: &mut Transaction<'_, Postgres>,
    recipient_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<NotificationWithActor>> {
    let notifications = sqlx::query!(
        r#"
        WITH scoped AS (
            SELECT
                id,
                actor_id,
                read_at,
                created_at,
                -- Reactions collapse per post; everything else is its own group,
                -- keyed by its own id so it can never merge with anything.
                CASE
                    WHEN notification_type = 'reaction' THEN 'reaction:' || post_id::text
                    ELSE 'single:' || id::text
                END AS group_key
            FROM notifications
            WHERE recipient_id = $1
        ),
        groups AS (
            SELECT
                MAX(created_at) AS latest_at,
                -- Distinct actors, not rows: one person reacting twice to the
                -- same drawing is one person, and the row says "and N others".
                COUNT(DISTINCT actor_id) AS actor_count,
                BOOL_OR(read_at IS NULL) AS has_unread,
                -- Ordered by created_at DESC, so element 1 is the newest member
                -- and MAX(created_at) above is its timestamp.
                (ARRAY_AGG(id ORDER BY created_at DESC))[1] AS representative_id
            FROM scoped
            GROUP BY group_key
        )
        SELECT
            groups.actor_count AS "actor_count!",
            groups.has_unread AS "has_unread!",
            n.id,
            n.recipient_id,
            n.actor_id,
            a.name AS actor_name,
            a.handle AS actor_handle,
            actor_users.login_name AS "actor_login_name?",
            n.notification_type as "notification_type: NotificationType",
            n.post_id,
            n.comment_id,
            n.reaction_iri,
            r.emoji AS "reaction_emoji?",
            n.guestbook_entry_id,
            n.read_at,
            n.created_at,
            p.title AS post_title,
            post_authors.login_name AS "post_author_login_name?",
            images.image_filename AS "post_image_filename?",
            images.width AS "post_image_width?",
            images.height AS "post_image_height?",
            c.content AS "comment_content?",
            c.content_html AS "comment_content_html?",
            g.content AS "guestbook_content?"
        FROM groups
        JOIN notifications n ON n.id = groups.representative_id
        LEFT JOIN actors a ON n.actor_id = a.id
        LEFT JOIN users actor_users ON a.user_id = actor_users.id
        LEFT JOIN posts p ON n.post_id = p.id
        LEFT JOIN users post_authors ON p.author_id = post_authors.id
        LEFT JOIN images ON p.image_id = images.id
        LEFT JOIN comments c ON n.comment_id = c.id
        LEFT JOIN reactions r ON n.reaction_iri = r.iri
        LEFT JOIN guestbook_entries g ON n.guestbook_entry_id = g.id
        ORDER BY groups.latest_at DESC, n.id
        LIMIT $2 OFFSET $3
        "#,
        recipient_id,
        limit,
        offset
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(notifications
        .into_iter()
        .map(|row| NotificationWithActor {
            id: row.id,
            recipient_id: row.recipient_id,
            actor_id: row.actor_id,
            actor_name: row.actor_name,
            actor_handle: row.actor_handle,
            actor_login_name: row.actor_login_name,
            notification_type: row.notification_type,
            post_id: row.post_id,
            comment_id: row.comment_id,
            reaction_iri: row.reaction_iri,
            reaction_emoji: row.reaction_emoji,
            guestbook_entry_id: row.guestbook_entry_id,
            // A group counts as unread while any member of it is, so marking it
            // read from the list clears the whole clump rather than the one
            // reaction that happened to be newest.
            read_at: if row.has_unread { None } else { row.read_at },
            created_at: row.created_at,
            post_title: row.post_title,
            post_author_login_name: row.post_author_login_name,
            post_image_filename: row.post_image_filename,
            post_image_width: row.post_image_width,
            post_image_height: row.post_image_height,
            comment_content: row.comment_content,
            comment_content_html: row.comment_content_html,
            guestbook_content: row.guestbook_content,
            actor_count: row.actor_count,
        })
        .collect())
}

/// Get a single notification by ID, carrying the same group count the list
/// gives it.
///
/// The mark-read handler re-renders whatever it just changed, and for a
/// collapsed reaction group that is the whole group — without the count here it
/// would swap "Alice and 15 others reacted" for a bare "Alice reacted" the
/// moment you marked it read.
pub async fn get_notification_by_id(
    tx: &mut Transaction<'_, Postgres>,
    notification_id: Uuid,
    recipient_id: Uuid,
) -> Result<Option<NotificationWithActor>> {
    let notification = sqlx::query!(
        r#"
        SELECT
            (
                SELECT COUNT(DISTINCT peer.actor_id)
                FROM notifications peer
                WHERE peer.recipient_id = n.recipient_id
                  AND n.notification_type = 'reaction'
                  AND peer.notification_type = 'reaction'
                  AND peer.post_id = n.post_id
            ) AS "peer_actor_count?",
            n.id,
            n.recipient_id,
            n.actor_id,
            a.name AS actor_name,
            a.handle AS actor_handle,
            actor_users.login_name AS "actor_login_name?",
            n.notification_type as "notification_type: NotificationType",
            n.post_id,
            n.comment_id,
            n.reaction_iri,
            r.emoji AS "reaction_emoji?",
            n.guestbook_entry_id,
            n.read_at,
            n.created_at,
            p.title AS post_title,
            post_authors.login_name AS "post_author_login_name?",
            images.image_filename AS "post_image_filename?",
            images.width AS "post_image_width?",
            images.height AS "post_image_height?",
            c.content AS "comment_content?",
            c.content_html AS "comment_content_html?",
            g.content AS "guestbook_content?"
        FROM notifications n
        LEFT JOIN actors a ON n.actor_id = a.id
        LEFT JOIN users actor_users ON a.user_id = actor_users.id
        LEFT JOIN posts p ON n.post_id = p.id
        LEFT JOIN users post_authors ON p.author_id = post_authors.id
        LEFT JOIN images ON p.image_id = images.id
        LEFT JOIN comments c ON n.comment_id = c.id
        LEFT JOIN reactions r ON n.reaction_iri = r.iri
        LEFT JOIN guestbook_entries g ON n.guestbook_entry_id = g.id
        WHERE n.id = $1 AND n.recipient_id = $2
        "#,
        notification_id,
        recipient_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(notification.map(|row| NotificationWithActor {
        id: row.id,
        recipient_id: row.recipient_id,
        actor_id: row.actor_id,
        actor_name: row.actor_name,
        actor_handle: row.actor_handle,
        actor_login_name: row.actor_login_name,
        notification_type: row.notification_type,
        post_id: row.post_id,
        comment_id: row.comment_id,
        reaction_iri: row.reaction_iri,
        reaction_emoji: row.reaction_emoji,
        guestbook_entry_id: row.guestbook_entry_id,
        read_at: row.read_at,
        created_at: row.created_at,
        post_title: row.post_title,
        post_author_login_name: row.post_author_login_name,
        post_image_filename: row.post_image_filename,
        post_image_width: row.post_image_width,
        post_image_height: row.post_image_height,
        comment_content: row.comment_content,
        comment_content_html: row.comment_content_html,
        guestbook_content: row.guestbook_content,
        // NULL for everything that is not a reaction, and 0 can't happen for one
        // (the row is its own peer), so 1 is the right floor either way.
        actor_count: row.peer_actor_count.unwrap_or(1).max(1),
    }))
}

/// Mark a notification as read, and with it the rest of its group.
///
/// The list collapses every reaction on a post into one row, so the one button
/// the reader can see has to clear all of them. Marking only the representative
/// would leave the group unread and it would spring back on the next load.
pub async fn mark_notification_as_read(
    tx: &mut Transaction<'_, Postgres>,
    notification_id: Uuid,
    recipient_id: Uuid,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        WITH target AS (
            SELECT id, notification_type, post_id
            FROM notifications
            WHERE id = $1 AND recipient_id = $2
        )
        UPDATE notifications n
        SET read_at = CURRENT_TIMESTAMP
        FROM target
        WHERE n.recipient_id = $2
          AND n.read_at IS NULL
          AND (
              n.id = target.id
              OR (
                  target.notification_type = 'reaction'
                  AND n.notification_type = 'reaction'
                  AND n.post_id = target.post_id
              )
          )
        "#,
        notification_id,
        recipient_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Mark all notifications as read for a user
pub async fn mark_all_notifications_as_read(
    tx: &mut Transaction<'_, Postgres>,
    recipient_id: Uuid,
) -> Result<u64> {
    let result = sqlx::query!(
        r#"
        UPDATE notifications
        SET read_at = CURRENT_TIMESTAMP
        WHERE recipient_id = $1 AND read_at IS NULL
        "#,
        recipient_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected())
}

/// Get unread notification count for a user
pub async fn get_unread_count(
    tx: &mut Transaction<'_, Postgres>,
    recipient_id: Uuid,
) -> Result<i64> {
    let result = sqlx::query!(
        r#"
        SELECT COUNT(*) as count
        FROM notifications
        WHERE recipient_id = $1 AND read_at IS NULL
        "#,
        recipient_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(result.count.unwrap_or(0))
}

/// Delete a notification, and with it the rest of its group.
///
/// Same reason as marking read: the row the reader deleted stands for every
/// reaction on that post, so leaving the others behind would delete one line
/// and put fifteen back in its place.
pub async fn delete_notification(
    tx: &mut Transaction<'_, Postgres>,
    notification_id: Uuid,
    recipient_id: Uuid,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        WITH target AS (
            SELECT id, notification_type, post_id
            FROM notifications
            WHERE id = $1 AND recipient_id = $2
        )
        DELETE FROM notifications n
        USING target
        WHERE n.recipient_id = $2
          AND (
              n.id = target.id
              OR (
                  target.notification_type = 'reaction'
                  AND n.notification_type = 'reaction'
                  AND n.post_id = target.post_id
              )
          )
        "#,
        notification_id,
        recipient_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// The page a push notification opens, as a path on the site: the post it is
/// about, or for a follow the follower, or for a guestbook the guestbook it
/// was written in. The notifications page when there is nothing better, which
/// is where every notification can be found anyway.
fn push_url(notification: &NotificationWithActor, recipient_login_name: Option<&str>) -> String {
    use NotificationType::*;
    match notification.notification_type {
        Comment | Reaction | Mention | PostReply | CommentReply | CommunityPost => {
            match (notification.post_id, &notification.post_author_login_name) {
                (Some(post_id), Some(author)) => format!("/@{author}/{post_id}"),
                (Some(post_id), None) => format!("/posts/{post_id}"),
                _ => "/notifications".to_string(),
            }
        }
        // A follower from another server has no login name here, and no page
        // of theirs on this site to open.
        Follow => match &notification.actor_login_name {
            Some(actor) => format!("/@{actor}"),
            None => "/notifications".to_string(),
        },
        // An entry is written in the recipient's own guestbook; a reply is the
        // actor answering one the recipient wrote in theirs.
        GuestbookEntry => match recipient_login_name {
            Some(recipient) => format!("/@{recipient}"),
            None => "/notifications".to_string(),
        },
        GuestbookReply => match &notification.actor_login_name {
            Some(actor) => format!("/@{actor}"),
            None => "/notifications".to_string(),
        },
    }
}

/// Send push notification for a newly created notification
/// This should be called after create_notification() succeeds and the transaction is committed
pub async fn send_push_for_notification(
    push_service: &crate::push::PushService,
    pool: &sqlx::PgPool,
    notification: &NotificationWithActor,
    badge_count: Option<u32>,
) {
    // Get recipient's preferred language
    let preferred_language = match pool.acquire().await {
        Ok(mut conn) => {
            match get_user_language_preference(&mut conn, notification.recipient_id).await {
                Ok(lang) => lang,
                Err(e) => {
                    tracing::warn!(
                        "Failed to get language preference for user {}: {:?}. Using English as fallback.",
                        notification.recipient_id,
                        e
                    );
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "Failed to acquire connection from pool: {:?}. Using English as fallback.",
                e
            );
            None
        }
    };

    // Format notification message based on type and user's language
    let (title, body) = format_notification_message(notification, preferred_language);

    let badge = badge_count;

    // Add custom data for deep linking
    let mut data = serde_json::Map::new();
    data.insert(
        "notification_id".to_string(),
        serde_json::json!(notification.id.to_string()),
    );
    data.insert(
        "notification_type".to_string(),
        serde_json::json!(format!("{:?}", notification.notification_type)),
    );

    // Where tapping it goes. The apps open this rather than working it out
    // from the type, so a new kind of notification, or a route that moves,
    // is right in every app without a release of any of them.
    let recipient_login_name = match notification.notification_type {
        NotificationType::GuestbookEntry => sqlx::query_scalar!(
            "SELECT login_name FROM users WHERE id = $1",
            notification.recipient_id
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten(),
        _ => None,
    };
    let url = push_url(notification, recipient_login_name.as_deref());

    if let Some(post_id) = notification.post_id {
        data.insert(
            "post_id".to_string(),
            serde_json::json!(post_id.to_string()),
        );
    }
    if let Some(comment_id) = notification.comment_id {
        data.insert(
            "comment_id".to_string(),
            serde_json::json!(comment_id.to_string()),
        );
    }
    if let Some(actor_login_name) = &notification.actor_login_name {
        data.insert(
            "actor_login_name".to_string(),
            serde_json::json!(actor_login_name),
        );
    }

    // Send push notification (don't fail if this errors)
    if let Err(e) = push_service
        .send_notification_to_user(notification.recipient_id, &title, &body, badge, &url, data)
        .await
    {
        tracing::warn!(
            "Failed to send push notification to user {}: {:?}",
            notification.recipient_id,
            e
        );
    }
}

/// Get user's language preference from database
pub async fn get_user_language_preference(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
) -> Result<Option<Language>> {
    let result = sqlx::query!(
        r#"
        SELECT preferred_language as "preferred_language: Language"
        FROM users
        WHERE id = $1
        "#,
        user_id
    )
    .fetch_one(conn)
    .await?;

    Ok(result.preferred_language)
}

/// Helper function to get a Fluent bundle for the given language
fn get_fluent_bundle(
    language: Option<Language>,
) -> FluentBundle<&'static FluentResource, IntlLangMemoizer> {
    let lang_code = match language {
        Some(Language::Ko) => "ko",
        Some(Language::Ja) => "ja",
        Some(Language::En) => "en",
        Some(Language::Zh) => "zh",
        None => "en", // Default to English
    };

    let ftl = LOCALES
        .get(lang_code)
        .unwrap_or_else(|| LOCALES.get("en").expect("English locale must exist"));

    let mut bundle = FluentBundle::new_concurrent(vec![lang_code
        .parse()
        .expect("Language code should be valid")]);
    bundle.add_resource(ftl).expect("Failed to add a resource.");
    bundle
}

/// Helper function to get a localized message from the bundle with optional arguments
fn get_localized_message(
    bundle: &FluentBundle<&FluentResource, IntlLangMemoizer>,
    key: &str,
    args: Option<&FluentArgs>,
) -> String {
    let message = bundle
        .get_message(key)
        .unwrap_or_else(|| panic!("Message {} not found", key));
    let pattern = message
        .value()
        .unwrap_or_else(|| panic!("Message {} has no value", key));
    let mut errors = vec![];
    bundle
        .format_pattern(pattern, args, &mut errors)
        .to_string()
}

/// Format a notification into a user-friendly push notification message
fn format_notification_message(
    notification: &NotificationWithActor,
    language: Option<Language>,
) -> (String, String) {
    let actor_name = &notification.actor_name;
    let bundle = get_fluent_bundle(language);

    let mut args = FluentArgs::new();
    args.set("name", actor_name.clone());

    match notification.notification_type {
        NotificationType::Comment => {
            let title =
                get_localized_message(&bundle, "push-notification-comment-title", Some(&args));

            let body = if let Some(content) = &notification.comment_content {
                content.clone()
            } else {
                get_localized_message(&bundle, "push-notification-comment-body", Some(&args))
            };
            (title, body)
        }
        NotificationType::Reaction => {
            let emoji = notification.reaction_emoji.as_deref().unwrap_or("❤️");
            args.set("emoji", emoji.to_string());

            let title =
                get_localized_message(&bundle, "push-notification-reaction-title", Some(&args));
            let body =
                get_localized_message(&bundle, "push-notification-reaction-body", Some(&args));
            (title, body)
        }
        NotificationType::Follow => {
            let title = get_localized_message(&bundle, "push-notification-follow-title", None);
            let body = get_localized_message(&bundle, "push-notification-follow-body", Some(&args));
            (title, body)
        }
        NotificationType::GuestbookEntry => {
            let title = get_localized_message(
                &bundle,
                "push-notification-guestbook-entry-title",
                Some(&args),
            );
            let body = notification.guestbook_content.clone().unwrap_or_default();
            (title, body)
        }
        NotificationType::GuestbookReply => {
            let title = get_localized_message(
                &bundle,
                "push-notification-guestbook-reply-title",
                Some(&args),
            );
            let body = notification.guestbook_content.clone().unwrap_or_default();
            (title, body)
        }
        NotificationType::Mention => {
            let title =
                get_localized_message(&bundle, "push-notification-mention-title", Some(&args));

            let body = if let Some(content) = &notification.comment_content {
                content.clone()
            } else {
                get_localized_message(&bundle, "push-notification-mention-body", Some(&args))
            };
            (title, body)
        }
        NotificationType::PostReply => {
            let title =
                get_localized_message(&bundle, "push-notification-post-reply-title", Some(&args));
            let body = notification.post_title.clone().unwrap_or_default();
            (title, body)
        }
        NotificationType::CommentReply => {
            let title = get_localized_message(
                &bundle,
                "push-notification-comment-reply-title",
                Some(&args),
            );

            let body = if let Some(content) = &notification.comment_content {
                content.clone()
            } else {
                get_localized_message(&bundle, "push-notification-comment-reply-body", Some(&args))
            };
            (title, body)
        }
        NotificationType::CommunityPost => {
            let title =
                get_localized_message(&bundle, "push-notification-community-post-title", None);

            let body = if let Some(post_title) = &notification.post_title {
                args.set("title", post_title.clone());
                get_localized_message(
                    &bundle,
                    "push-notification-community-post-body-with-title",
                    Some(&args),
                )
            } else {
                get_localized_message(
                    &bundle,
                    "push-notification-community-post-body",
                    Some(&args),
                )
            };
            (title, body)
        }
    }
}

/// Format a community invitation-related message for push notifications
/// Returns a tuple of (title, body) localized for the user's language preference
pub fn format_community_invitation_message(
    notification_type: &str, // "invite", "accepted", "declined"
    language: Option<Language>,
    inviter_name: &str,
    community_slug: &str,
) -> (String, String) {
    let bundle = get_fluent_bundle(language);
    let mut args = FluentArgs::new();

    match notification_type {
        "invite" => {
            args.set("inviter", inviter_name.to_string());
            args.set("community", community_slug.to_string());

            let title =
                get_localized_message(&bundle, "push-notification-community-invite-title", None);
            let body = get_localized_message(
                &bundle,
                "push-notification-community-invite-body",
                Some(&args),
            );
            (title, body)
        }
        "accepted" => {
            args.set("accepter", inviter_name.to_string());
            args.set("community", community_slug.to_string());

            let title =
                get_localized_message(&bundle, "push-notification-invite-accepted-title", None);
            let body = get_localized_message(
                &bundle,
                "push-notification-invite-accepted-body",
                Some(&args),
            );
            (title, body)
        }
        "declined" => {
            args.set("decliner", inviter_name.to_string());
            args.set("community", community_slug.to_string());

            let title =
                get_localized_message(&bundle, "push-notification-invite-declined-title", None);
            let body = get_localized_message(
                &bundle,
                "push-notification-invite-declined-body",
                Some(&args),
            );
            (title, body)
        }
        _ => {
            // Fallback to English for unknown types
            (
                "Notification".to_string(),
                "You have a new notification".to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(kind: NotificationType) -> NotificationWithActor {
        NotificationWithActor {
            id: Uuid::nil(),
            recipient_id: Uuid::nil(),
            actor_id: Uuid::nil(),
            actor_name: "Actor".to_string(),
            actor_handle: "@actor".to_string(),
            actor_login_name: Some("actor".to_string()),
            notification_type: kind,
            post_id: None,
            comment_id: None,
            reaction_iri: None,
            reaction_emoji: None,
            guestbook_entry_id: None,
            read_at: None,
            created_at: Utc::now(),
            post_title: None,
            post_author_login_name: None,
            post_image_filename: None,
            post_image_width: None,
            post_image_height: None,
            comment_content: None,
            comment_content_html: None,
            guestbook_content: None,
            actor_count: 1,
        }
    }

    #[test]
    fn a_push_opens_what_it_is_about() {
        let post_id = Uuid::parse_str("9c881320-2b43-4afa-b2bb-7128c8a3e985").unwrap();
        let mut comment = notification(NotificationType::Comment);
        comment.post_id = Some(post_id);
        assert_eq!(push_url(&comment, None), format!("/posts/{post_id}"));
        comment.post_author_login_name = Some("author".to_string());
        assert_eq!(push_url(&comment, None), format!("/@author/{post_id}"));

        assert_eq!(
            push_url(&notification(NotificationType::Follow), None),
            "/@actor"
        );
        assert_eq!(
            push_url(&notification(NotificationType::GuestbookEntry), Some("me")),
            "/@me"
        );
        assert_eq!(
            push_url(&notification(NotificationType::GuestbookReply), None),
            "/@actor"
        );
    }

    #[test]
    fn a_push_with_nowhere_better_opens_the_notifications() {
        assert_eq!(
            push_url(&notification(NotificationType::Reaction), None),
            "/notifications"
        );
        assert_eq!(
            push_url(&notification(NotificationType::GuestbookEntry), None),
            "/notifications"
        );
        let mut remote = notification(NotificationType::Follow);
        remote.actor_login_name = None;
        assert_eq!(push_url(&remote, None), "/notifications");
    }
}
