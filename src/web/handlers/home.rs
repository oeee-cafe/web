use super::ExtractFtlLang;
use crate::app_error::AppError;
use crate::feed_period;
use crate::models::actor::Actor;
use crate::models::comment::{find_recent_comments, CommentScope, NotificationComment};
use crate::models::post::{
    find_following_posts_by_user_id, find_member_community_posts, find_public_posts,
};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::state::AppState;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{extract::State, response::Html};
use axum_messages::Messages;
use serde::Deserialize;
use uuid::Uuid;

use minijinja::context;

/// Posts fetched per batch on the home grid.
///
/// Has to comfortably exceed one screenful at the *densest* thumbnail size, or
/// the infinite-scroll sentinel starts already visible and chain-loads batches
/// until the viewport finally fills. At the smallest card size on a wide
/// monitor a screen holds roughly a hundred cards, so this bounds the worst
/// case to a single extra fetch rather than eliminating it.
pub(crate) const HOME_POSTS_PER_BATCH: i64 = 60;

/// Comments fetched a batch, beside a feed (comments_aside_macro.jinja) and
/// on a list of them (comments_fragment.jinja). More than a wide window's
/// column holds, so its sentinel starts out of sight.
pub(crate) const COMMENTS_PER_BATCH: i64 = 30;

/// Context for comments_fragment.jinja: a batch of comments, and where the
/// next one comes from while there may be one -- a full batch -- keyed by
/// the last comment in this one (find_recent_comments).
pub(crate) fn comments_context(rows: Vec<NotificationComment>, batch_path: &str) -> minijinja::Value {
    let next_url = match rows.last() {
        Some(last) if rows.len() as i64 == COMMENTS_PER_BATCH => {
            Some(format!("{}?after={}", batch_path, last.id))
        }
        _ => None,
    };
    context! { rows, next_url }
}

/// A batch of comments in `scope`, as `viewer` may see them.
pub(crate) async fn comments_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: CommentScope,
    viewer: Option<&crate::models::user::User>,
    after: Option<Uuid>,
) -> Result<Vec<NotificationComment>, AppError> {
    Ok(find_recent_comments(
        tx,
        scope,
        viewer.map(|user| user.id),
        viewer.map_or(false, |user| user.show_sensitive_content),
        after,
        COMMENTS_PER_BATCH,
    )
    .await?)
}

/// A comment list's next batch: `after` is the last comment of the one
/// before.
#[derive(Deserialize)]
pub struct CommentsQuery {
    pub after: Option<Uuid>,
}

/// Context every post feed hands to the shared card fragment. Home's feeds and
/// the collaborate lobby differ only in which query fills `posts` and where the
/// sentinel points, so everything else lives here rather than being written
/// three times.
///
/// Every one of them is newest first, so the grid is broken up by month
/// (feed_period.rs): `headings` holds, for each post, the heading it opens,
/// if any. `after` is the month the previous batch ended in, which the
/// sentinel hands on as `period`, so a batch that carries on a month does
/// not head it a second time.
pub(crate) fn feed_context(
    posts: Vec<crate::models::post::SerializablePostForHome>,
    fragment_path: &str,
    offset: i64,
    after: Option<&str>,
) -> minijinja::Value {
    let now = chrono::Utc::now();
    let headings = feed_period::headings(posts.iter().map(|post| post.published_at), after, now);
    let last_period = posts
        .iter()
        .rev()
        .find_map(|post| post.published_at)
        .map(|then| feed_period::period(then, now).key)
        .or_else(|| after.map(str::to_string));
    let has_more = posts.len() as i64 == HOME_POSTS_PER_BATCH;
    let next_offset = offset + HOME_POSTS_PER_BATCH;
    let mut next_url = format!(
        "{}?offset={}&limit={}",
        fragment_path, next_offset, HOME_POSTS_PER_BATCH
    );
    if let Some(key) = last_period {
        // "2026-08": nothing to encode.
        next_url.push_str("&period=");
        next_url.push_str(&key);
    }
    context! {
        posts,
        headings,
        has_more,
        next_url,
    }
}

/// Home's feeds: one grid of drawings in four orders, switched between
/// by the pill where the feed's heading would be (feed_switch.jinja). Each
/// is its own address and its own batch endpoint for the infinite scroll.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feed {
    /// `/`: every public drawing, newest first.
    Recent,
    /// `/following`: the people the reader follows.
    Following,
    /// `/joined`: the communities the reader is a member of.
    Communities,
}

impl Feed {
    /// What feed_switch.jinja calls it.
    fn name(self) -> &'static str {
        match self {
            Feed::Recent => "recent",
            Feed::Following => "following",
            Feed::Communities => "communities",
        }
    }

    /// Where the next batch comes from.
    fn batch_path(self) -> &'static str {
        match self {
            Feed::Recent => "/api/home/posts",
            Feed::Following => "/api/following/posts",
            Feed::Communities => "/api/joined/posts",
        }
    }

    /// Whose comments go with it: the same people's, or places', as its
    /// drawings. Following and Communities are the reader's own, so nobody
    /// signed in has either.
    fn comment_scope(
        self,
        viewer: Option<&crate::models::user::User>,
    ) -> Result<CommentScope, AppError> {
        match (self, viewer) {
            (Feed::Recent, _) => Ok(CommentScope::Public),
            (Feed::Following, Some(user)) => Ok(CommentScope::FollowedBy(user.id)),
            (Feed::Communities, Some(user)) => Ok(CommentScope::MemberOf(user.id)),
            _ => Err(AppError::Unauthorized),
        }
    }

    /// Its comments, as a page of their own (feed_comments_page).
    fn comments_path(self) -> &'static str {
        match self {
            Feed::Recent => "/comments",
            Feed::Following => "/following/comments",
            Feed::Communities => "/joined/comments",
        }
    }

    /// Where the next batch of its comments comes from.
    fn comments_batch_path(self) -> &'static str {
        match self {
            Feed::Recent => "/api/home/comments",
            Feed::Following => "/api/following/comments",
            Feed::Communities => "/api/joined/comments",
        }
    }

    /// One batch. Following and Communities are the reader's own, so
    /// nobody signed in has either.
    async fn posts(
        self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        viewer: Option<&crate::models::user::User>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<crate::models::post::SerializablePostForHome>, AppError> {
        let viewer_id = viewer.map(|user| user.id);
        let show_sensitive = viewer.map_or(false, |user| user.show_sensitive_content);
        Ok(match self {
            Feed::Recent => find_public_posts(tx, limit, offset, viewer_id, show_sensitive).await?,
            Feed::Following => {
                let user = viewer.ok_or(AppError::Unauthorized)?;
                find_following_posts_by_user_id(tx, user.id, show_sensitive, limit, offset).await?
            }
            Feed::Communities => {
                let user = viewer.ok_or(AppError::Unauthorized)?;
                find_member_community_posts(tx, user.id, show_sensitive, limit, offset).await?
            }
        })
    }
}

/// A feed's page: its first batch in the one template all four share.
async fn feed_page(
    feed: Feed,
    auth_session: AuthSession,
    state: AppState,
    ftl_lang: String,
    messages: Messages,
) -> Result<axum::response::Response, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    let posts = feed
        .posts(&mut tx, auth_session.user.as_ref(), HOME_POSTS_PER_BATCH, 0)
        .await?;
    let scope = feed.comment_scope(auth_session.user.as_ref())?;
    let comments = comments_batch(&mut tx, scope, auth_session.user.as_ref(), None).await?;
    tx.commit().await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("home.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        feed_switch => feed.name(),
        feed_view => "drawings",
        feed => feed_context(posts, feed.batch_path(), 0, None),
        comments => comments_context(comments, feed.comments_batch_path()),
        comments_url => feed.comments_path(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;
    Ok(Html(rendered).into_response())
}

/// A feed's comments as a page of their own, loading as it is scrolled: where
/// a phone, which has no room beside the grid for them, goes on to from the
/// few it shows above it, and the other half of the drawings | comments
/// switch at every width.
async fn feed_comments_page(
    feed: Feed,
    auth_session: AuthSession,
    state: AppState,
    ftl_lang: String,
    messages: Messages,
) -> Result<axum::response::Response, AppError> {
    let scope = feed.comment_scope(auth_session.user.as_ref())?;
    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    let comments = comments_batch(&mut tx, scope, auth_session.user.as_ref(), None).await?;
    tx.commit().await?;

    let rendered = state.env.get_template("home_comments.jinja")?.render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        feed_switch => feed.name(),
        feed_view => "comments",
        comments => comments_context(comments, feed.comments_batch_path()),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;
    Ok(Html(rendered).into_response())
}

/// A feed's next batch of comments, and the sentinel for the one after.
async fn feed_comments_batch(
    feed: Feed,
    auth_session: AuthSession,
    state: AppState,
    ftl_lang: String,
    query: CommentsQuery,
) -> Result<axum::response::Response, AppError> {
    let scope = feed.comment_scope(auth_session.user.as_ref())?;
    let mut tx = state.db_pool.begin().await?;
    let comments = comments_batch(&mut tx, scope, auth_session.user.as_ref(), query.after).await?;
    tx.commit().await?;

    let rendered = state.env.get_template("comments_fragment.jinja")?.render(context! {
        comments => comments_context(comments, feed.comments_batch_path()),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;
    Ok(Html(rendered).into_response())
}

/// GET /comments
pub async fn recent_comments_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_page(Feed::Recent, auth_session, state, ftl_lang, messages).await
}

/// GET /following/comments
pub async fn following_comments_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_page(Feed::Following, auth_session, state, ftl_lang, messages).await
}

/// GET /joined/comments
pub async fn joined_comments_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_page(Feed::Communities, auth_session, state, ftl_lang, messages).await
}

/// GET /api/home/comments
pub async fn load_more_recent_comments(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<CommentsQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_batch(Feed::Recent, auth_session, state, ftl_lang, query).await
}

/// GET /api/following/comments
pub async fn load_more_following_comments(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<CommentsQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_batch(Feed::Following, auth_session, state, ftl_lang, query).await
}

/// GET /api/joined/comments
pub async fn load_more_joined_comments(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<CommentsQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_comments_batch(Feed::Communities, auth_session, state, ftl_lang, query).await
}

/// A feed's next batch of cards, and the sentinel for the one after.
async fn feed_batch(
    feed: Feed,
    auth_session: AuthSession,
    state: AppState,
    ftl_lang: String,
    query: LoadMoreQuery,
) -> Result<axum::response::Response, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let posts = feed
        .posts(&mut tx, auth_session.user.as_ref(), query.limit, query.offset)
        .await?;
    tx.commit().await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("post_feed_fragment.jinja")?;
    let rendered = template.render(context! {
        feed => feed_context(posts, feed.batch_path(), query.offset, query.period.as_deref()),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;
    Ok(Html(rendered).into_response())
}

pub async fn home(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_page(Feed::Recent, auth_session, state, ftl_lang, messages).await
}

pub async fn my_timeline(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_page(Feed::Following, auth_session, state, ftl_lang, messages).await
}

pub async fn my_communities_feed(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_page(Feed::Communities, auth_session, state, ftl_lang, messages).await
}

#[derive(Deserialize)]
pub struct LoadMoreQuery {
    pub offset: i64,
    pub limit: i64,
    /// The month the previous batch ended in (feed_context).
    pub period: Option<String>,
}

pub async fn load_more_public_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Recent, auth_session, state, ftl_lang, query).await
}

/// GET /api/following/posts
pub async fn load_more_timeline_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Following, auth_session, state, ftl_lang, query).await
}

/// GET /api/joined/posts
pub async fn load_more_community_feed_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Communities, auth_session, state, ftl_lang, query).await
}

pub async fn do_delete_comment(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(comment_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = match auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let comment_uuid = Uuid::parse_str(&comment_id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Find the comment
    let comment = sqlx::query!(
        r#"
        SELECT id, actor_id, deleted_at
        FROM comments
        WHERE id = $1
        "#,
        comment_uuid
    )
    .fetch_optional(&mut *tx)
    .await?;

    let comment = match comment {
        Some(c) => c,
        None => return Ok(StatusCode::NOT_FOUND.into_response()),
    };

    // Check if comment is already deleted
    if comment.deleted_at.is_some() {
        return Ok(StatusCode::GONE.into_response());
    }

    // Check if the user is the comment author
    if comment.actor_id != actor.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Delete the comment
    crate::models::comment::delete_comment(
        &mut tx,
        comment_uuid,
        crate::models::comment::CommentDeletionReason::UserDeleted,
    )
    .await?;

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct AddReactionRequest {
    // emoji comes from the URL path
}

#[cfg(test)]
mod tests {
    use chrono::Datelike;
    use crate::models::comment::NotificationComment;
    use crate::models::post::SerializablePostForHome;
    use crate::web::handlers::test_support;
    use minijinja::context;
    use serde_json::json;

    fn sample_post() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "title": "A drawing",
            "user_login_name": "someone",
            "community_slug": "open",
            "image_filename": "abcdef.png",
            "image_width": 300,
            "image_height": 300,
            "is_sensitive": false,
            "community_name": "Open Studio",
            "published_at": "2026-01-02T03:04:05Z",
        })
    }

    fn home_context(posts: Vec<serde_json::Value>, has_more: bool) -> minijinja::Value {
        context! {
            feed => context! {
                posts => posts.clone(),
                has_more => has_more,
                next_url => format!(
                    "/api/home/posts?offset={}&limit={}",
                    super::HOME_POSTS_PER_BATCH,
                    super::HOME_POSTS_PER_BATCH
                ),
            },
            current_user => json!(null),
            feed_switch => "recent",
            feed_view => "drawings",
            comments => super::comments_context(Vec::new(), "/api/home/comments"),
            comments_url => "/comments",
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    #[test]
    fn renders_home_with_the_per_row_control() {
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(home_context(vec![sample_post()], false))
            .expect("home.jinja renders");
        assert!(rendered.contains("id=\"post-feed-grid\""));
        assert!(rendered.contains("class=\"feed-header\""));
        // The reader chooses how many a row, as /admin/posts does, and the
        // grid keeps that number at every width.
        assert!(rendered.contains("id=\"post-cols\""));
        // The readout is what tells the reader the control did something.
        assert!(rendered.contains("<output class=\"ds-per-row-value\" for=\"post-cols\">"));
        // Applied before the grid paints, from the one stored value.
        assert!(rendered.contains("homeCols"));
        // The grid opts into the wide container; the page — and so the header
        // above it — keeps the one width every other page uses.
        assert!(rendered.contains("class=\"center-wide\""));
        assert!(!rendered.contains("--page-width"));
    }

    /// Home's feeds are orders of one feed, so they are a switch where its
    /// heading was rather than tabs in the toolbar -- for someone signed in.
    /// Signed out there is only Recent, headed by its drawings | comments
    /// pill.
    #[test]
    fn home_switches_between_its_feeds_in_place_of_a_heading() {
        let env = test_support::env();
        let home = env.get_template("home.jinja").expect("template loads");

        let signed_out = home
            .render(home_context(vec![sample_post()], false))
            .expect("home.jinja renders");
        assert!(!signed_out.contains("feed-switch"), "one feed signed out, no switch");
        assert!(signed_out.contains(r#"<a href="/" aria-current="page">feed-view-drawings</a>"#));
        assert!(signed_out.contains(r#"<a href="/comments">feed-view-comments</a>"#));

        let signed_in = |feed_switch: &str| {
            home.render(context! {
                current_user => json!({"login_name": "someone"}),
                feed_switch,
                ..home_context(vec![sample_post()], false)
            })
            .expect("home.jinja renders")
        };
        let recent = signed_in("recent");
        assert!(recent.contains(r#"<a href="/following">feed-following</a>"#));
        assert!(recent.contains(r#"<a href="/joined">feed-communities</a>"#));
        for (feed, path) in [
            ("following", "/following"),
            ("communities", "/joined"),
        ] {
            let page = signed_in(feed);
            assert!(
                page.contains(&format!(r#"<a href="{path}" aria-current="page">feed-{feed}</a>"#)),
                "{feed} is the one marked"
            );
            assert!(page.contains(r#"<a href="/">feed-recent</a>"#));
        }
        // Home is the one section for all of them, and the tags are a
        // section of their own.
        assert!(recent.contains(r#"<a href="/tags">tag-discovery</a>"#));
    }

    /// A drawing fills its square, cropped from its longer side, so it is
    /// scaled down -- smoothly -- only when its shorter side is larger than
    /// the square; anything else keeps hard pixels, which only work upward.
    #[test]
    fn only_drawings_larger_than_the_square_scale_smoothly() {
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let mut large = sample_post();
        large["image_width"] = json!(550);
        large["image_height"] = json!(550);
        let rendered = template
            .render(home_context(vec![sample_post()], false))
            .expect("renders at 300");
        assert!(!rendered.contains("drawing-downscaled"));
        let rendered = template
            .render(home_context(vec![large], false))
            .expect("renders at 550");
        assert!(rendered.contains("drawing-downscaled"));
        // Wide but no taller than the square: it fills the square at 1x.
        let mut wide = sample_post();
        wide["image_width"] = json!(550);
        let rendered = template
            .render(home_context(vec![wide], false))
            .expect("renders at 550x300");
        assert!(!rendered.contains("drawing-downscaled"));
    }

    #[test]
    fn sentinel_batch_size_matches_the_handler() {
        // Regression: the sentinel URL used to hardcode limit=18 while the
        // handler fetched its own count. If they drift, the grid either skips
        // posts or re-fetches ones already shown.
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(home_context(vec![sample_post()], true))
            .expect("renders");
        // next_url is built in Rust and passed through {{ }}, so autoescaping
        // encodes `/` and `&`. That is correct HTML — the parser decodes them
        // before htmx reads the attribute. Pinned so double-escaping is caught.
        let expected = format!(
            "&#x2f;api&#x2f;home&#x2f;posts?offset={}&amp;limit={}",
            super::HOME_POSTS_PER_BATCH,
            super::HOME_POSTS_PER_BATCH
        );
        assert!(rendered.contains(&expected), "sentinel url drifted");
    }

    #[test]
    fn no_sentinel_when_there_is_no_more() {
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(home_context(vec![sample_post()], false))
            .expect("renders");
        assert!(!rendered.contains("infinite-scroll-sentinel"));
    }

    #[test]
    fn cards_link_to_their_community() {
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(home_context(vec![sample_post()], false))
            .expect("renders");
        assert!(rendered.contains("/communities/@open"));
        assert!(rendered.contains("Open Studio"));
        // Title, author handle and timestamp are credited too, not just the
        // community — the point is that attribution is visible, not implied.
        assert!(rendered.contains("A drawing"));
        assert!(rendered.contains("@someone"));
        // When is how long ago, with the full date as its tooltip.
        assert!(rendered.contains("post-card-when"));
    }

    #[test]
    fn cards_without_a_community_get_no_label() {
        // Posts can have no community at all; the label must not render an
        // empty link in that case.
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let mut post = sample_post();
        post["community_slug"] = json!(null);
        post["community_name"] = json!(null);
        let rendered = template
            .render(home_context(vec![post], false))
            .expect("renders");
        // The author is always credited, so the byline still renders — only the
        // community link inside it is conditional.
        assert!(rendered.contains("post-card-byline"));
        assert!(!rendered.contains("/communities/@"));
        // ...and the post link falls back to the author handle.
        assert!(rendered.contains("/@someone/"));
    }

    #[test]
    fn following_matches_the_home_feed_chrome() {
        // /following and / share the head, controls, grid id and card fragment; the
        // only difference is where the sentinel points.
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                feed_switch => "following",
                messages => Vec::<serde_json::Value>::new(),
                feed => context! {
                    posts => vec![sample_post()],
                    has_more => true,
                    next_url => "/api/following/posts?offset=60&limit=60",
                },
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("home.jinja renders");
        assert!(rendered.contains("class=\"center-wide\""));
        assert!(!rendered.contains("--page-width"));
        assert!(rendered.contains("id=\"post-cols\""));
        assert!(rendered.contains("id=\"post-feed-grid\""));
        assert!(rendered.contains("class=\"feed-header\""));
        assert!(rendered.contains("post-card-byline"));
        // Sentinel must target the timeline endpoint, not the public feed.
        assert!(rendered.contains("&#x2f;api&#x2f;following&#x2f;posts"));
        assert!(!rendered.contains("api&#x2f;home&#x2f;posts"));
    }

    #[test]
    fn an_empty_feed_says_so_without_the_control() {
        let env = test_support::env();
        let template = env.get_template("home.jinja").expect("template loads");
        let rendered = template
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                feed_switch => "communities",
                messages => Vec::<serde_json::Value>::new(),
                feed => context! {
                    posts => Vec::<serde_json::Value>::new(),
                    has_more => false,
                    next_url => "",
                },
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("renders");
        assert!(rendered.contains("feed-communities-empty"));
        assert!(!rendered.contains("id=\"post-cols\""));
    }

    /// A post as the feed queries return it, published at `published_at`.
    fn feed_post(i: u128, published_at: chrono::DateTime<chrono::Utc>) -> SerializablePostForHome {
        SerializablePostForHome {
            id: uuid::Uuid::from_u128(i),
            title: Some(format!("Drawing {i}")),
            author_id: uuid::Uuid::from_u128(999),
            user_login_name: "artist".to_string(),
            paint_duration: "0".to_string(),
            stroke_count: 1,
            viewer_count: 0,
            image_filename: "abcdef.png".to_string(),
            image_width: 300,
            image_height: 300,
            replay_filename: None,
            is_sensitive: false,
            community_slug: None,
            community_name: None,
            published_at: Some(published_at),
            created_at: published_at,
            updated_at: published_at,
        }
    }

    fn render_fragment(feed: minijinja::Value) -> String {
        test_support::env()
            .get_template("post_feed_fragment.jinja")
            .expect("template loads")
            .render(context! {
                feed,
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders")
    }

    /// The grid is broken up by month, with one heading over the first
    /// drawing of each, rendered from what feed_context really hands the
    /// template.
    #[test]
    fn a_heading_opens_each_month() {
        let now = chrono::Utc::now();
        let posts = vec![
            feed_post(1, now),
            feed_post(2, now),
            feed_post(3, now - chrono::Duration::days(400)),
        ];
        let rendered = render_fragment(super::feed_context(posts, "/api/home/posts", 0, None));
        assert_eq!(rendered.matches(r#"class="feed-period""#).count(), 2);
        // This month, by its number alone...
        let this_month = format!(
            "feed-period-month(month={})",
            now.with_timezone(&chrono_tz::Asia::Seoul).month()
        );
        assert!(rendered.contains(&this_month), "{rendered}");
        // ...and a year ago, a month of another year, which says the year.
        assert!(rendered.contains("feed-period-month-year(month="));
        // The heading comes before the drawing it opens.
        let heading = rendered.find(&this_month).unwrap();
        let first = rendered.find("Drawing 1").unwrap();
        assert!(heading < first);
    }

    /// A batch that carries on the month the last one ended in does not
    /// repeat its heading, and hands on where it ended for the next one.
    #[test]
    fn a_batch_does_not_repeat_the_heading_above_it() {
        let now = chrono::Utc::now();
        let month = crate::feed_period::period(now, now).key;
        let posts: Vec<_> = (0..super::HOME_POSTS_PER_BATCH as u128)
            .map(|i| feed_post(i + 1, now))
            .collect();
        let rendered = render_fragment(super::feed_context(
            posts,
            "/api/home/posts",
            60,
            Some(&month),
        ));
        assert!(!rendered.contains("feed-period"), "the same month, no new heading");
        assert!(
            rendered.contains(&format!("&amp;period={month}")),
            "the next batch is told"
        );
    }

    fn sample_comment() -> NotificationComment {
        let at = chrono::Utc::now();
        NotificationComment {
            id: uuid::Uuid::from_u128(10),
            post_id: uuid::Uuid::from_u128(1),
            actor_id: uuid::Uuid::from_u128(11),
            content: Some("Lovely colours".to_string()),
            content_html: None,
            iri: None,
            actor_name: "Commenter".to_string(),
            actor_handle: "@commenter@oeee.test".to_string(),
            actor_url: "https://oeee.test/@commenter".to_string(),
            actor_login_name: Some("commenter".to_string()),
            is_local: true,
            updated_at: at,
            created_at: at,
            post_title: Some("A drawing".to_string()),
            post_author_login_name: "someone".to_string(),
            post_image_filename: Some("abcdef.png".to_string()),
            post_image_width: Some(300),
            post_image_height: Some(300),
        }
    }

    /// What people are saying goes beside the grid, in the section that
    /// lays the two out; with nothing said, the grid has the width alone.
    #[test]
    fn comments_go_beside_the_grid_when_there_are_any() {
        let env = test_support::env();
        let home = env.get_template("home.jinja").expect("template loads");
        let with = home
            .render(context! {
                comments => super::comments_context(vec![sample_comment()], "/api/home/comments"),
                ..home_context(vec![sample_post()], false)
            })
            .expect("renders with comments");
        // The toolbar's skeleton of this page (toolbar.jinja) carries the
        // same classes in a script, so these look for what only the page
        // itself says.
        let aside_tag = r#"<aside class="feed-comments" aria-labelledby="feed-comments-title">"#;
        assert!(with.contains(r#"<section class="feed-layout has-comments" role="region""#));
        assert!(with.contains(aside_tag));
        assert!(with.contains("Lovely colours"));
        assert!(with.contains("/@someone/00000000-0000-0000-0000-000000000001"));
        // One comment is all there is, so a phone, which shows three, has
        // nowhere further to go, and there is no next batch.
        assert!(!with.contains("feed-comments-more"));
        assert!(!with.contains("infinite-scroll-sentinel"));
        // Header, then comments, then the grid: the order it reads in.
        let aside = with.find(aside_tag).unwrap();
        assert!(with.find(r#"<div class="feed-header">"#).unwrap() < aside);
        assert!(aside < with.find("post-feed-grid").unwrap());

        let without = home
            .render(home_context(vec![sample_post()], false))
            .expect("renders without comments");
        assert!(!without.contains(aside_tag));
        assert!(without.contains(r#"<section class="feed-layout" role="region""#));
    }

    /// A full batch beside the grid loads the next from after its last
    /// comment, and a phone, which shows the first three, is sent on to the
    /// feed's comments page.
    #[test]
    fn a_full_batch_of_comments_loads_on_and_links_to_the_rest() {
        let rows: Vec<NotificationComment> = (0..super::COMMENTS_PER_BATCH as u128)
            .map(|i| NotificationComment {
                id: uuid::Uuid::from_u128(100 + i),
                ..sample_comment()
            })
            .collect();
        let last = uuid::Uuid::from_u128(100 + super::COMMENTS_PER_BATCH as u128 - 1);
        let rendered = test_support::env()
            .get_template("home.jinja")
            .expect("template loads")
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                feed_switch => "following",
                comments => super::comments_context(rows, "/api/following/comments"),
                comments_url => "/following/comments",
                ..home_context(vec![sample_post()], false)
            })
            .expect("renders");
        assert!(rendered.contains(&format!(
            r#"hx-get="&#x2f;api&#x2f;following&#x2f;comments?after={last}""#
        )));
        assert!(rendered.contains(
            r#"<a class="feed-comments-more" href="&#x2f;following&#x2f;comments">"#
        ));
    }

    /// A feed's comments are a page of their own, the other half of its
    /// drawings | comments pill, and switching feeds there keeps to
    /// comments. Each pill is named for the other's choice, so a morph
    /// replaces the one not pressed rather than keeping its old links.
    #[test]
    fn a_feeds_comments_are_a_page_that_keeps_to_comments() {
        let env = test_support::env();
        let page = |feed_switch: &str, feed_view: &str, template: &str| {
            env.get_template(template)
                .expect("template loads")
                .render(context! {
                    current_user => json!({"login_name": "someone"}),
                    feed_switch,
                    feed_view,
                    comments => super::comments_context(vec![sample_comment()], "/api/joined/comments"),
                    ..home_context(vec![sample_post()], false)
                })
                .expect("renders")
        };
        let comments = page("communities", "comments", "home_comments.jinja");
        assert!(comments.contains(r#"<div class="comment-grid">"#));
        assert!(comments.contains("Lovely colours"));
        assert!(!comments.contains(r#"id="post-feed-grid""#), "no drawings under it");
        assert!(comments.contains(r#"<a href="/joined/comments" aria-current="page">feed-communities</a>"#));
        assert!(comments.contains(r#"<a href="/following/comments">feed-following</a>"#));
        assert!(comments.contains(r#"<a href="/comments">feed-recent</a>"#));
        assert!(comments.contains(r#"<a href="/joined">feed-view-drawings</a>"#));
        assert!(comments.contains(r#"<a href="/joined/comments" aria-current="page">feed-view-comments</a>"#));
        assert!(comments.contains(r#"id="feed-switch-comments""#));
        assert!(comments.contains(r#"id="feed-views-communities""#));

        let drawings = page("communities", "drawings", "home.jinja");
        assert!(drawings.contains(r#"id="feed-switch-drawings""#));
        assert!(drawings.contains(r#"<a href="/joined/comments">feed-view-comments</a>"#));
    }

    #[test]
    fn fragment_sentinel_carries_the_limit_through() {
        let env = test_support::env();
        let template = env
            .get_template("post_feed_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                feed => context! {
                    posts => vec![sample_post()],
                    has_more => true,
                    next_url => "/api/home/posts?offset=120&limit=60",
                },
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");
        assert!(rendered.contains("&#x2f;api&#x2f;home&#x2f;posts?offset=120&amp;limit=60"));
    }
}
