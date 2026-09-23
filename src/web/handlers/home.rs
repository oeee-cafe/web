use super::ExtractFtlLang;
use crate::app_error::AppError;
use crate::models::actor::Actor;
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

/// Context every post feed hands to the shared card fragment. Home's feeds and
/// the collaborate lobby differ only in which query fills `posts` and where the
/// sentinel points, so everything else lives here rather than being written
/// three times.
pub(crate) fn feed_context(
    posts: Vec<crate::models::post::SerializablePostForHome>,
    fragment_path: &str,
    offset: i64,
) -> minijinja::Value {
    let has_more = posts.len() as i64 == HOME_POSTS_PER_BATCH;
    let next_offset = offset + HOME_POSTS_PER_BATCH;
    context! {
        posts,
        has_more,
        next_url => format!(
            "{}?offset={}&limit={}",
            fragment_path, next_offset, HOME_POSTS_PER_BATCH
        ),
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
    tx.commit().await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("home.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        feed_switch => feed.name(),
        feed => feed_context(posts, feed.batch_path(), 0),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;
    Ok(Html(rendered).into_response())
}

/// A feed's next batch of cards, and the sentinel for the one after.
async fn feed_batch(
    feed: Feed,
    auth_session: AuthSession,
    state: AppState,
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
        feed => feed_context(posts, feed.batch_path(), query.offset),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
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
}

pub async fn load_more_public_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Recent, auth_session, state, query).await
}

/// GET /api/following/posts
pub async fn load_more_timeline_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Following, auth_session, state, query).await
}

/// GET /api/joined/posts
pub async fn load_more_community_feed_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Communities, auth_session, state, query).await
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
    /// Signed out there is only Recent, which keeps its heading.
    #[test]
    fn home_switches_between_its_feeds_in_place_of_a_heading() {
        let env = test_support::env();
        let home = env.get_template("home.jinja").expect("template loads");

        let signed_out = home
            .render(home_context(vec![sample_post()], false))
            .expect("home.jinja renders");
        assert!(!signed_out.contains("feed-switch"), "one feed signed out, no switch");
        assert!(signed_out.contains(r#"<h2 class="home-section-title">recent-drawings</h2>"#));

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
