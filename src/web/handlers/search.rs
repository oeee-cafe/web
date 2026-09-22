use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::handlers::ExtractFtlLang;
use crate::web::responses::{SearchPostResult, SearchResponse};
use crate::web::state::AppState;
use axum::extract::Query;
use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse};
use axum::{extract::State, response::Json};
use chrono::{DateTime, Utc};
use minijinja::context;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Matches of each kind shown on `/search`, which has no load-more: the most
/// the JSON endpoint will hand out in one go.
const SEARCH_PAGE_LIMIT: i64 = 50;

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SearchPageQuery {
    #[serde(default)]
    q: Option<String>,
}

/// A matching post, with what `post_card.jinja` needs to draw it as well as
/// the few fields the JSON endpoint returns.
#[derive(Serialize)]
pub struct SearchPostRow {
    pub id: Uuid,
    pub title: Option<String>,
    pub user_login_name: String,
    pub image_filename: Option<String>,
    pub image_width: Option<i32>,
    pub image_height: Option<i32>,
    pub is_sensitive: bool,
    pub community_slug: Option<String>,
    pub community_name: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

/// Posts by title or content, as the viewer is allowed to see them. Shared by `/search` and `/api/v1/search`
/// so the page and the app's JSON cannot disagree about what matches.
pub async fn search(
    tx: &mut Transaction<'_, Postgres>,
    q: &str,
    limit: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<SearchPostRow>, AppError> {
    let search_term = format!("%{}%", q);

    // Only posts from public communities, or from none.
    let posts = sqlx::query_as!(
        SearchPostRow,
        r#"
        SELECT
            posts.id,
            posts.title,
            users.login_name AS user_login_name,
            images.image_filename AS "image_filename?",
            images.width AS "image_width?",
            images.height AS "image_height?",
            (posts.is_sensitive OR posts.is_explicit) AS "is_sensitive!",
            communities.slug AS "community_slug?",
            communities.name AS "community_name?",
            posts.published_at
        FROM posts
        LEFT JOIN users ON posts.author_id = users.id
        LEFT JOIN images ON posts.image_id = images.id
        LEFT JOIN communities ON posts.community_id = communities.id
        WHERE (posts.title ILIKE $1 OR posts.content ILIKE $1)
          AND posts.published_at IS NOT NULL
          AND posts.deleted_at IS NULL
          AND (communities.visibility = 'public' OR posts.community_id IS NULL)
          AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $3 = true OR posts.author_id = $4)
        ORDER BY posts.published_at DESC
        LIMIT $2
        "#,
        search_term,
        limit,
        viewer_show_sensitive,
        viewer_user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(posts)
}

fn viewer(auth_session: &AuthSession) -> (Option<Uuid>, bool) {
    match auth_session.user {
        Some(ref user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    }
}

pub async fn search_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let limit = query.limit.unwrap_or(20).min(50);
    let (viewer_user_id, viewer_show_sensitive) = viewer(&auth_session);

    let posts = search(
        &mut tx,
        &query.q,
        limit,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    tx.commit().await?;

    // Minimal fields for thumbnails
    let posts_typed: Vec<SearchPostResult> = posts
        .into_iter()
        .map(|post| {
            let image_url = if let Some(ref filename) = post.image_filename {
                let image_prefix = &filename[..2];
                format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, filename
                )
            } else {
                String::new()
            };

            SearchPostResult {
                id: post.id,
                image_url,
                image_width: post.image_width,
                image_height: post.image_height,
                is_sensitive: post.is_sensitive,
            }
        })
        .collect();

    Ok(Json(SearchResponse { posts: posts_typed }))
}

/// What the iOS and Android apps add to their web views' user agent.
const APP_USER_AGENT_MARKERS: [&str; 2] = ["OeeeCafeiOS", "OeeeCafeAndroid"];

/// Whether the page is shown in one of the apps, whose search tab has a native
/// search field above the page.
fn has_native_search_field(headers: &HeaderMap) -> bool {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|ua| APP_USER_AGENT_MARKERS.iter().any(|marker| ua.contains(marker)))
}

/// GET /search — drawings matching `q`, under the form that asked.
///
/// A missing or blank `q` is the form alone rather than a 404, since the apps'
/// search tabs and a bare visit both land here with nothing typed yet. The apps
/// bring their own search field, so they get the results without the form.
pub async fn search_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    headers: HeaderMap,
    Query(params): Query<SearchPageQuery>,
) -> Result<impl IntoResponse, AppError> {
    let search_query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_owned);

    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let posts = match search_query {
        Some(ref q) => {
            let (viewer_user_id, viewer_show_sensitive) = viewer(&auth_session);
            let posts = search(
                &mut tx,
                q,
                SEARCH_PAGE_LIMIT,
                viewer_user_id,
                viewer_show_sensitive,
            )
            .await?;
            // A card is a picture; a post without one has nothing to show.
            posts
                .into_iter()
                .filter(|post| post.image_filename.is_some())
                .collect()
        }
        None => Vec::new(),
    };
    tx.commit().await?;

    let template = state.env.get_template("search.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        search_query,
        native_search_field => has_native_search_field(&headers),
        posts,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}
