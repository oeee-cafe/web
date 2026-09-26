use crate::app_error::AppError;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Redirect},
};
use axum_messages::Messages;
use chrono::{DateTime, Utc};
use minijinja::context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    models::{
        community::get_pending_invitations_with_details_for_user,
        notification::{
            delete_notification, get_badge_count, get_notification_by_id,
            list_notifications as fetch_notifications, mark_all_notifications_as_read,
            mark_notification_as_read, notification_url, NotificationWithActor,
        },
        user::AuthSession,
    },
    web::{
        context::CommonContext, handlers::ExtractFtlLang, responses::UnreadCountResponse,
        state::AppState,
    },
};

/// Rows per batch in the notification list.
///
/// This used to be a hardcoded 50 with no way to ask for the next page, so a
/// reader with more than fifty simply could not reach the rest — two people on
/// this instance were already past it, one at 74.
pub const NOTIFICATIONS_PER_BATCH: i64 = 30;

/// GET /api/notifications/items — one batch of notification rows plus the next
/// sentinel, for htmx to swap in.
pub async fn notifications_fragment(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<NotificationsFragmentQuery>,
) -> Result<Html<String>, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let offset = query.offset.unwrap_or(0).max(0);

    let mut tx = state.db_pool.begin().await?;
    let notifications =
        fetch_notifications(&mut tx, user.id, NOTIFICATIONS_PER_BATCH, offset).await?;
    tx.commit().await?;

    let has_more = notifications.len() as i64 == NOTIFICATIONS_PER_BATCH;
    let opened = query
        .opened
        .and_then(DateTime::<Utc>::from_timestamp_micros);

    let template = state.env.get_template("notifications_fragment.jinja")?;
    let rendered = template.render(context! {
        notifications => as_shown(notifications, opened),
        has_more => has_more,
        next_url => notifications_fragment_url(offset + NOTIFICATIONS_PER_BATCH, opened),
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct NotificationsFragmentQuery {
    /// Row offset for the infinite-scroll sentinel. The first batch omits it.
    pub offset: Option<i64>,
    /// When the page these rows are scrolled into was rendered, in
    /// microseconds since the epoch. Viewing the page marks everything read
    /// (`mark_notifications_seen`), so a row read since then was unread when
    /// the reader arrived and is still shown as new.
    pub opened: Option<i64>,
}

/// URL the infinite-scroll sentinel fetches next.
fn notifications_fragment_url(next_offset: i64, opened: Option<DateTime<Utc>>) -> String {
    match opened {
        Some(opened) => format!(
            "/api/notifications/items?offset={next_offset}&opened={}",
            opened.timestamp_micros()
        ),
        None => format!("/api/notifications/items?offset={next_offset}"),
    }
}

/// A row as the list shows it: `unread` is whether it was unread when the
/// page was opened, which is not `read_at`, because opening the page is what
/// reads it.
#[derive(Serialize)]
struct ShownNotification {
    #[serde(flatten)]
    notification: NotificationWithActor,
    unread: bool,
}

fn as_shown(
    notifications: Vec<NotificationWithActor>,
    opened: Option<DateTime<Utc>>,
) -> Vec<ShownNotification> {
    notifications
        .into_iter()
        .map(|notification| {
            let unread = match (notification.read_at, opened) {
                (None, _) => true,
                (Some(read_at), Some(opened)) => read_at >= opened,
                (Some(_), None) => false,
            };
            ShownNotification {
                notification,
                unread,
            }
        })
        .collect()
}

pub async fn list_notifications(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = auth_session
        .user
        .as_ref()
        .ok_or(AppError::Unauthorized)?
        .clone();

    let opened = Utc::now();
    let notifications = fetch_notifications(&mut tx, user.id, NOTIFICATIONS_PER_BATCH, 0).await?;
    let has_more = notifications.len() as i64 == NOTIFICATIONS_PER_BATCH;

    // Fetch pending invitations with all details in a single query (no N+1)
    let invitations = get_pending_invitations_with_details_for_user(&mut tx, user.id).await?;

    let invitations_with_details: Vec<serde_json::Value> = invitations
        .into_iter()
        .map(|invitation| {
            serde_json::json!({
                "id": invitation.id,
                "community_name": invitation.community_name,
                "community_slug": invitation.community_slug,
                "inviter_login_name": invitation.inviter_login_name,
                "inviter_display_name": invitation.inviter_display_name,
                "created_at": invitation.created_at,
            })
        })
        .collect();

    // Get common context (includes unread_notification_count and draft_post_count)
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    // Unread notifications, not counting the invitations the bell adds to
    // them: whether the page has anything for `mark_notifications_seen` to do.
    let unseen = common_ctx.unread_notification_count > invitations_with_details.len() as i64;

    let template: minijinja::Template<'_, '_> = state.env.get_template("notifications.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        notifications => as_shown(notifications, Some(opened)),
        unseen => unseen,
        invitations => invitations_with_details,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        // Same key names the fragment uses, so the first batch and every
        // scrolled batch render through one template.
        has_more => has_more,
        next_url => notifications_fragment_url(NOTIFICATIONS_PER_BATCH, Some(opened)),
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

/// Render the header's notification link, wrapped as an `<hx-partial>`.
///
/// Every action on this page changes the unread count, and the count lives in
/// the site header, outside whatever the action targeted. htmx 4's
/// `<hx-partial>` carries its own target, so a handler can hand back the row
/// it was asked for *and* the corrected badge in one response, and the number
/// stops drifting from the list it counts.
pub(crate) async fn nav_notification_badge(
    state: &AppState,
    user_id: Uuid,
    ftl_lang: &str,
) -> Result<String, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let unread = get_badge_count(&mut tx, user_id).await?;
    tx.commit().await?;

    let template = state.env.get_template("nav_notifications.jinja")?;
    let rendered = template.render(context! {
        unread_notification_count => unread,
        ftl_lang,
    })?;
    Ok(format!(
        "<hx-partial hx-target=\"#nav-notifications\" hx-swap=\"outerHTML\">{rendered}</hx-partial>"
    ))
}

/// GET /notifications/{id}/open — where a notification's push and toast
/// link to: it marks the notification read, and with it the rest of its
/// reaction group, then sends the reader on to the page it is about.
///
/// Opening a notification is reading it. Before this, only the notifications
/// page's buttons marked anything read, so a push tapped and followed stayed
/// unread and kept the apps' icons badged for something already seen.
pub async fn open_notification(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(notification_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let mut tx = state.db_pool.begin().await?;
    let Some(notification) = get_notification_by_id(&mut tx, notification_id, user.id).await?
    else {
        // Deleted since, or someone else's: the list is where it would have been.
        tx.rollback().await?;
        return Ok(Redirect::to("/notifications"));
    };
    let marked = mark_notification_as_read(&mut tx, notification_id, user.id).await?;
    tx.commit().await?;

    if marked {
        state.push_service.refresh_badge(user.id);
    }
    Ok(Redirect::to(&notification_url(
        &notification,
        &user.login_name,
    )))
}

/// POST /notifications/seen — sent by the notifications page once it is on
/// screen (notifications.jinja), and it marks every notification read: the
/// list is where they are all read, so having looked at it is having read
/// them.
///
/// From the page rather than from the handler that renders it, because htmx
/// preloads a boosted link at the press, and a page fetched and never shown
/// has not been seen. The rows keep their unread look for the visit
/// (`ShownNotification`); the answer is only the bell, corrected.
pub async fn mark_notifications_seen(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
) -> Result<impl IntoResponse, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let mut tx = state.db_pool.begin().await?;
    let marked = mark_all_notifications_as_read(&mut tx, user.id).await?;
    tx.commit().await?;

    if marked > 0 {
        state.push_service.refresh_badge(user.id);
    }
    let badge = nav_notification_badge(&state, user.id, &ftl_lang).await?;
    Ok(Html(badge))
}

/// The number on the bell for the current user: unread notifications and
/// pending invitations together (`get_badge_count`).
pub async fn get_unread_notification_count(
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<UnreadCountResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = auth_session
        .user
        .as_ref()
        .ok_or(AppError::Unauthorized)?
        .clone();

    let count = get_badge_count(&mut tx, user.id).await?;

    tx.commit().await?;

    Ok(Json(UnreadCountResponse { count }))
}

/// Delete a specific notification
pub async fn delete_notification_handler(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(notification_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = auth_session
        .user
        .as_ref()
        .ok_or(AppError::Unauthorized)?
        .clone();

    let success = delete_notification(&mut tx, notification_id, user.id).await?;

    tx.commit().await?;

    if success {
        state.push_service.refresh_badge(user.id);
        // Empty main content removes the row; the partial alongside it fixes
        // the badge, which was counting a notification that no longer exists.
        let badge = nav_notification_badge(&state, user.id, &ftl_lang).await?;
        Ok(Html(badge).into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, Html("".to_string())).into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::{as_shown, notifications_fragment_url};
    use crate::web::handlers::test_support;
    use minijinja::context;
    use serde_json::json;

    fn sample_notification() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "notification_type": "Follow",
            "read_at": null,
            "unread": true,
            "created_at": "2026-01-02T03:04:05Z",
            "actor_login_name": "someone",
            "actor_name": "Someone",
            "actor_count": 1,
        })
    }

    /// A reaction to a titled post: the common case, and the one that carries a
    /// thumbnail. 55% of all notifications are reactions.
    fn sample_reaction() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "notification_type": "Reaction",
            "read_at": "2026-01-02T04:00:00Z",
            "unread": false,
            "created_at": "2026-01-02T03:04:05Z",
            "actor_login_name": "someone",
            "actor_name": "Someone",
            "reaction_emoji": "\u{1f49c}",
            "post_id": "00000000-0000-0000-0000-000000000009",
            "post_title": "A drawing",
            "post_author_login_name": "artist",
            "post_image_filename": "abcdef.png",
            "post_image_width": 300,
            "post_image_height": 300,
            "actor_count": 1,
        })
    }

    /// Sixteen people reacting to one drawing is one row. The biggest real
    /// group on this instance is fifteen.
    fn sample_reaction_group() -> serde_json::Value {
        let mut n = sample_reaction();
        n["actor_count"] = json!(16);
        n["read_at"] = json!(null);
        n["unread"] = json!(true);
        n
    }

    fn sample_invitation() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "community_name": "Open Studio",
            "community_slug": "open",
            "inviter_login_name": "someone",
            "inviter_display_name": "Someone",
            "created_at": "2026-01-02T03:04:05Z",
        })
    }

    fn render(
        notifications: Vec<serde_json::Value>,
        invitations: Vec<serde_json::Value>,
    ) -> String {
        render_seen(notifications, invitations, false)
    }

    fn render_seen(
        notifications: Vec<serde_json::Value>,
        invitations: Vec<serde_json::Value>,
        unseen: bool,
    ) -> String {
        let env = test_support::env();
        let template = env
            .get_template("notifications.jinja")
            .expect("template loads");
        template
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                messages => Vec::<serde_json::Value>::new(),
                notifications => notifications,
                invitations => invitations,
                draft_post_count => 0,
                unread_notification_count => 1,
                unseen => unseen,
                has_more => false,
                next_url => "/api/notifications/items?offset=30",
                ftl_lang => "en",
            })
            .expect("notifications.jinja renders")
    }

    /// The list used to carry an <h3> holding the same string as the page's
    /// <h2>, so the page opened with its own title printed twice.
    #[test]
    fn the_page_title_is_not_repeated_over_the_list() {
        let rendered = render(vec![sample_notification()], Vec::new());
        // ftl_get_message is stubbed to echo the id, so both headings would
        // render the literal key.
        // The page's content only: the toolbar's shortcuts list names the
        // page too, and that is not a second heading.
        let content = rendered.split("<main class=").nth(1).expect("a <main>");
        assert_eq!(
            content.matches(">notifications<").count(),
            1,
            "notifications heading rendered more than once"
        );
        assert!(rendered.contains("notifications-title"));
    }

    /// The invitations block keeps its heading: it is the section that is not
    /// notifications, so it is the one that needs naming.
    #[test]
    fn invitations_keep_their_own_heading() {
        let rendered = render(vec![sample_notification()], vec![sample_invitation()]);
        assert!(rendered.contains("invitations-pending"));
        let content = rendered.split("<main class=").nth(1).expect("a <main>");
        assert_eq!(content.matches(">notifications<").count(), 1);
        assert!(rendered.contains("Open Studio"));
    }

    /// The row is one line: actor and verb in a single <p>, with the type
    /// label gone. It used to be a button bar, a type label restating the verb
    /// below it, then the actor and the action as two separate paragraphs.
    #[test]
    fn a_notification_is_one_row_not_four() {
        let rendered = render(vec![sample_reaction()], Vec::new());
        assert_eq!(rendered.matches("notification-line").count(), 1);
        // The type label ("New reaction") sat directly above the sentence that
        // already said it.
        assert!(!rendered.contains("notification-type"));
        assert!(!rendered.contains("notification-header"));
        assert!(!rendered.contains("notification-body"));
        // Actor and verb are in the same paragraph now.
        assert!(rendered.contains("notification-actor"));
        assert!(rendered.contains("notification-action"));
    }

    /// Regression: every title-bearing type wrapped its whole action line in
    /// `if post_title`, so a reaction to an untitled drawing rendered the
    /// actor's name followed by nothing at all.
    #[test]
    fn an_untitled_post_still_gets_a_verb() {
        let mut untitled = sample_reaction();
        untitled["post_title"] = json!(null);
        let rendered = render(vec![untitled], Vec::new());
        assert!(
            rendered.contains("notification-action"),
            "untitled post rendered an actor with no verb"
        );
        // Falls back to the same string the post cards use rather than a new
        // one. The stub echoes both the pattern id and the arguments, so this
        // sees the title that was actually interpolated.
        assert!(
            rendered.contains("postTitle=post-untitled"),
            "the untitled fallback did not reach the action pattern"
        );
    }

    /// 95% of notifications carry a post. The thumbnail used to be gated on a
    /// hardcoded list of six type names instead of on having an image.
    #[test]
    fn anything_with_a_post_image_gets_a_thumbnail() {
        let rendered = render(vec![sample_reaction()], Vec::new());
        assert!(rendered.contains("notification-post-image"));
        assert!(rendered.contains("/image/ab/abcdef.png"));
        // Follows have no post, so no thumbnail and no broken image.
        let follow = render(vec![sample_notification()], Vec::new());
        assert!(!follow.contains("notification-post-image"));
    }

    /// Opening the page reads every row on it, so a row has no "mark read",
    /// and none of it is left for a "mark all read" either. What was unread
    /// when the page opened still looks it, for the visit.
    #[test]
    fn rows_show_what_was_unread_and_offer_only_delete() {
        let rendered = render(vec![sample_notification(), sample_reaction()], Vec::new());
        assert_eq!(rendered.matches("class=\"notification unread\"").count(), 1);
        assert!(!rendered.contains("mark-read"));
        assert!(!rendered.contains("mark-all-read"));
        assert!(rendered.contains("notification-delete"));
    }

    /// The page marks everything read once it is on screen, and only when
    /// there is something to mark.
    #[test]
    fn the_page_reports_itself_seen_only_with_something_unread() {
        let unseen = render_seen(vec![sample_notification()], Vec::new(), true);
        assert!(unseen.contains("hx-post=\"/notifications/seen\""));
        assert!(unseen.contains("hx-trigger=\"load\""));

        let seen = render_seen(vec![sample_reaction()], Vec::new(), false);
        assert!(!seen.contains("/notifications/seen"));
    }

    /// A row read after the page opened was read by opening it: it was new to
    /// the reader and is shown so, in the first batch and in every one
    /// scrolled in after it.
    #[test]
    fn a_row_read_by_opening_the_page_is_still_shown_as_new() {
        use chrono::{Duration, Utc};
        let opened = Utc::now();
        let row = |read_at| crate::models::notification::NotificationWithActor {
            id: uuid::Uuid::nil(),
            recipient_id: uuid::Uuid::nil(),
            actor_id: uuid::Uuid::nil(),
            actor_name: "Someone".to_string(),
            actor_handle: "@someone".to_string(),
            actor_login_name: Some("someone".to_string()),
            notification_type: crate::models::notification::NotificationType::Follow,
            post_id: None,
            comment_id: None,
            reaction_iri: None,
            reaction_emoji: None,
            guestbook_entry_id: None,
            read_at,
            created_at: opened - Duration::days(2),
            post_title: None,
            post_author_login_name: None,
            post_image_filename: None,
            post_image_width: None,
            post_image_height: None,
            comment_content: None,
            comment_content_html: None,
            guestbook_content: None,
            actor_count: 1,
        };
        let shown = as_shown(
            vec![
                row(None),
                row(Some(opened + Duration::seconds(1))),
                row(Some(opened - Duration::days(1))),
            ],
            Some(opened),
        );
        let unread: Vec<bool> = shown.iter().map(|s| s.unread).collect();
        assert_eq!(unread, vec![true, true, false]);

        // Without an opening time, only what is unread now is.
        let shown = as_shown(vec![row(None), row(Some(opened))], None);
        let unread: Vec<bool> = shown.iter().map(|s| s.unread).collect();
        assert_eq!(unread, vec![true, false]);
    }

    /// The invitation row is built from the same pieces as a notification row,
    /// but its wording is a sentence frame the locales fill in four parts —
    /// "Invitation from" @who "to join" Community — so all four must survive.
    #[test]
    fn invitations_keep_their_sentence_frame() {
        let rendered = render(Vec::new(), vec![sample_invitation()]);
        for key in ["invitation-from", "invitation-to-community"] {
            assert!(rendered.contains(key), "{key} dropped from the invitation");
        }
        assert!(rendered.contains("@someone"));
        assert!(rendered.contains("Open Studio"));
        // Both answers are offered. The test environment prints message ids,
        // so these are the buttons' labels.
        assert!(rendered.contains(">invitation-accept<"));
        assert!(rendered.contains(">invitation-reject<"));
    }

    /// Sixteen reactions on one drawing are one row saying so, not sixteen
    /// rows saying it one at a time.
    #[test]
    fn a_reaction_group_names_one_actor_and_counts_the_rest() {
        let rendered = render(vec![sample_reaction_group()], Vec::new());
        assert_eq!(rendered.matches("notification-line").count(), 1);
        assert!(rendered.contains("Someone"));
        // 16 actors: one named, fifteen counted.
        assert!(
            rendered.contains("count=15"),
            "the group did not count the other actors"
        );
        // A group has as many emoji as it has people, so the grouped verb drops
        // it rather than picking one.
        assert!(rendered.contains("notification-action-reacted-to-post-grouped"));
        assert!(!rendered.contains("emoji="));
    }

    /// One person reacting keeps the emoji and gains no "and 0 others".
    #[test]
    fn a_single_reaction_is_unchanged() {
        let rendered = render(vec![sample_reaction()], Vec::new());
        assert!(rendered.contains("emoji=\u{1f49c}"));
        assert!(!rendered.contains("notification-actors-and-others"));
        assert!(!rendered.contains("notification-action-reacted-to-post-grouped"));
    }

    /// The list was capped at 50 with no way to ask for more; two readers here
    /// were already past it. The sentinel is what makes the rest reachable.
    #[test]
    fn a_full_batch_offers_the_next_one() {
        let env = test_support::env();
        let template = env
            .get_template("notifications_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                notifications => vec![sample_reaction()],
                has_more => true,
                next_url => "/api/notifications/items?offset=30",
                ftl_lang => "en",
            })
            .expect("fragment renders standalone");
        assert!(rendered.contains("infinite-scroll-sentinel"));
        assert!(rendered.contains("hx-trigger=\"revealed\""));
        // Built in Rust and passed through {{ }}, so autoescaping encodes the
        // slashes. Pinned so double-escaping is caught.
        assert!(rendered.contains("&#x2f;api&#x2f;notifications&#x2f;items?offset=30"));
    }

    #[test]
    fn a_short_batch_is_the_end_of_the_list() {
        let env = test_support::env();
        let template = env
            .get_template("notifications_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                notifications => vec![sample_reaction()],
                has_more => false,
                next_url => "",
                ftl_lang => "en",
            })
            .expect("renders");
        assert!(!rendered.contains("infinite-scroll-sentinel"));
    }

    /// The sentinel offset has to match what the page already rendered, or the
    /// second batch either skips rows or repeats them.
    #[test]
    fn the_first_sentinel_starts_where_the_page_stopped() {
        assert_eq!(
            notifications_fragment_url(super::NOTIFICATIONS_PER_BATCH, None),
            "/api/notifications/items?offset=30"
        );
        let opened = chrono::DateTime::from_timestamp_micros(1_790_000_000_123_456);
        assert_eq!(
            notifications_fragment_url(super::NOTIFICATIONS_PER_BATCH, opened),
            "/api/notifications/items?offset=30&opened=1790000000123456"
        );
    }

    #[test]
    fn empty_state_shows_only_when_there_is_nothing_at_all() {
        let rendered = render(Vec::new(), Vec::new());
        assert!(rendered.contains("no-notifications"));
        // An invitation is something; the empty state must not claim otherwise.
        let with_invite = render(Vec::new(), vec![sample_invitation()]);
        assert!(!with_invite.contains("no-notifications"));
    }
}
