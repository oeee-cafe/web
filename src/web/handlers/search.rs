use crate::app_error::AppError;
use crate::models::handle::LoginName;
use crate::models::post::SerializablePostForHome;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::Query;
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use minijinja::context;
use minijinja::value::Serde;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Matches of each kind shown on `/search`, which has no load-more.
const SEARCH_PAGE_LIMIT: i64 = 50;

/// People shown over the drawings. A handle is one person, but the match
/// ignores case and handles are told apart by it, so there can be a few.
const SEARCH_PEOPLE_LIMIT: i64 = 5;

/// A person whose handle or name matches, as the profile's follow card
/// (person_card_macro.jinja) draws them.
#[derive(Serialize)]
pub struct SearchPersonRow {
    pub login_name: LoginName,
    pub display_name: String,
    pub banner_image_filename: Option<String>,
}

/// The person whose handle this is, `@` or not, whatever its case -- and
/// nobody else. Search does not list people by part of a name or a handle:
/// finding someone takes knowing who they are. The quick switcher finds
/// people the same way (jump.rs). A banner staff have flagged, or one taken
/// down, is left off the card, as on /about.
pub async fn search_people(
    tx: &mut Transaction<'_, Postgres>,
    q: &str,
    limit: i64,
) -> Result<Vec<SearchPersonRow>, AppError> {
    let escaped = crate::models::tag::escape_like(q.trim_start_matches('@'));
    let people = sqlx::query_as!(
        SearchPersonRow,
        r#"
        SELECT
            u.login_name,
            u.display_name,
            i.image_filename AS "banner_image_filename?"
        FROM users u
        LEFT JOIN banners b ON b.id = u.banner_id
            AND b.deleted_at IS NULL
            AND b.is_explicit = false
        LEFT JOIN images i ON i.id = b.image_id
        WHERE u.deleted_at IS NULL
          AND u.login_name ILIKE $1 ESCAPE '\'
        ORDER BY u.login_name ASC
        LIMIT $2
        "#,
        escaped,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(people)
}

#[derive(Deserialize)]
pub struct SearchPageQuery {
    #[serde(default)]
    q: Option<String>,
}

/// Posts by title or content, as the viewer is allowed to see them: the
/// same rows every feed draws its cards from, so a card here is a card
/// anywhere, jar and all.
pub async fn search(
    tx: &mut Transaction<'_, Postgres>,
    q: &str,
    limit: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<SerializablePostForHome>, AppError> {
    let search_term = format!("%{}%", q);

    // Only posts from public communities, or from none.
    let rows = sqlx::query!(
        r#"
        SELECT
            posts.id,
            posts.title,
            posts.author_id,
            users.login_name,
            images.paint_duration,
            images.stroke_count,
            images.image_filename,
            images.width,
            images.height,
            images.replay_filename,
            posts.viewer_count,
            (posts.is_sensitive OR posts.is_explicit) AS "is_sensitive!",
            communities.slug AS "community_slug?",
            communities.name AS "community_name?",
            posts.published_at,
            posts.created_at,
            posts.updated_at,
            EXISTS (SELECT 1 FROM pickles WHERE pickles.post_id = posts.id) AS "pickled!"
        FROM posts
        JOIN users ON posts.author_id = users.id
        JOIN images ON posts.image_id = images.id
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

    Ok(rows
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: row.community_slug,
            community_name: row.community_name,
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            pickled: row.pickled,
        })
        .collect())
}

fn viewer(auth_session: &AuthSession) -> (Option<Uuid>, bool) {
    match auth_session.user {
        Some(ref user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    }
}

/// GET /search — drawings matching `q`, under the form that asked.
///
/// A missing or blank `q` is the form alone rather than a 404, since a bare
/// visit lands here with nothing typed yet.
pub async fn search_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<SearchPageQuery>,
) -> Result<impl IntoResponse, AppError> {
    let search_query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_owned);

    let mut tx = state.db_pool.begin().await?;
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let people = match search_query {
        Some(ref q) => search_people(&mut tx, q, SEARCH_PEOPLE_LIMIT).await?,
        None => Vec::new(),
    };
    let posts = match search_query {
        Some(ref q) => {
            let (viewer_user_id, viewer_show_sensitive) = viewer(&auth_session);
            search(
                &mut tx,
                q,
                SEARCH_PAGE_LIMIT,
                viewer_user_id,
                viewer_show_sensitive,
            )
            .await?
        }
        None => Vec::new(),
    };
    tx.commit().await?;

    let rendered = state
        .render_page(
            "search.jinja",
            common_ctx,
            context! {
                search_query,
                people => Serde(people),
                posts => Serde(posts),
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}
