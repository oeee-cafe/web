use crate::app_error::AppError;
use crate::models::hashtag::{
    browse_hashtags, find_hashtag_by_name, find_posts_by_hashtag, normalize_hashtag,
    search_hashtags, Hashtag, HashtagSort,
};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::handlers::home::{feed_context, HOME_POSTS_PER_BATCH};
use crate::web::handlers::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use minijinja::context;
use serde::Deserialize;
use sqlx::{Postgres, Transaction};

/// Tags listed on the directory, and matches returned by a search.
const HASHTAG_LIST_LIMIT: i64 = 100;

/// Suggestions offered under the tag field while someone types.
const HASHTAG_AUTOCOMPLETE_LIMIT: i64 = 10;

/// Where `/hashtags/:name` and its load-more endpoint agree on a tag.
///
/// The path segment is normalized the same way the tag field normalizes what
/// was typed into it, so `/hashtags/Art`, `/hashtags/art` and `/hashtags/#art`
/// are one page rather than three, one of which 404s.
enum Requested {
    /// The path already spells the tag the way it is stored.
    Canonical(String),
    /// It does not; send the reader to the spelling that does.
    Elsewhere(String),
}

fn canonicalize(requested: &str) -> Requested {
    let normalized = normalize_hashtag(requested);
    if normalized == requested {
        Requested::Canonical(normalized)
    } else {
        Requested::Elsewhere(normalized)
    }
}

/// `/hashtags/<name>`, with the name escaped. Tags are letters, digits and
/// underscores now, so this only ever has non-ASCII to encode — but it is the
/// difference between a link that works for 그림 and one that depends on the
/// browser guessing.
fn hashtag_url(name: &str) -> String {
    format!("/hashtags/{}", urlencoding::encode(name))
}

/// The 404 page, rather than the bare `<h1>Hashtag not found</h1>` string this
/// used to answer with: unstyled, untranslated, and outside the site chrome.
async fn hashtag_not_found(
    tx: &mut Transaction<'_, Postgres>,
    state: &AppState,
    auth_session: &AuthSession,
    ftl_lang: &str,
) -> Result<axum::response::Response, AppError> {
    let common_ctx = CommonContext::build(tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    let template = state.env.get_template("404.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;
    Ok((StatusCode::NOT_FOUND, Html(rendered)).into_response())
}

/// GET /hashtags/:hashtag_name — one tag's drawings.
pub async fn hashtag_view(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(requested): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let name = match canonicalize(&requested) {
        Requested::Canonical(name) => name,
        Requested::Elsewhere(name) if name.is_empty() => {
            let mut tx = state.db_pool.begin().await?;
            let response = hashtag_not_found(&mut tx, &state, &auth_session, &ftl_lang).await?;
            tx.commit().await?;
            return Ok(response);
        }
        Requested::Elsewhere(name) => {
            return Ok(Redirect::permanent(&hashtag_url(&name)).into_response())
        }
    };

    let mut tx = state.db_pool.begin().await?;

    let Some(hashtag) = find_hashtag_by_name(&mut tx, &name).await? else {
        let response = hashtag_not_found(&mut tx, &state, &auth_session, &ftl_lang).await?;
        tx.commit().await?;
        return Ok(response);
    };

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    // The total comes back from the same query as the posts, over the same
    // filter, so the count in the heading is the number of drawings below it.
    let (posts, post_count) = find_posts_by_hashtag(
        &mut tx,
        &name,
        HOME_POSTS_PER_BATCH,
        0,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    let template = state.env.get_template("hashtag_view.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        hashtag => hashtag,
        post_count,
        feed => feed_context(posts, &format!("{}/posts", hashtag_url(&name)), 0),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct LoadMoreQuery {
    offset: i64,
    limit: i64,
}

/// GET /hashtags/:hashtag_name/posts — the next batch of cards for the tag
/// page's infinite scroll. Same fragment every other feed loads.
pub async fn load_more_hashtag_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(requested): Path<String>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    let name = normalize_hashtag(&requested);

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let mut tx = state.db_pool.begin().await?;
    let (posts, _) = find_posts_by_hashtag(
        &mut tx,
        &name,
        query.limit.clamp(1, HOME_POSTS_PER_BATCH),
        query.offset.max(0),
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    tx.commit().await?;

    let rendered = state
        .env
        .get_template("post_feed_fragment.jinja")?
        .render(context! {
            feed => feed_context(posts, &format!("{}/posts", hashtag_url(&name)), query.offset),
            r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct AutocompleteQuery {
    q: String,
}

/// GET /api/hashtags/autocomplete — suggestions under the tag field.
pub async fn hashtag_autocomplete(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<AutocompleteQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Nothing typed is nothing to suggest. The field fires on every keystroke
    // including the one that empties it, and an empty query used to match every
    // tag on the site and drop the menu open over the form.
    let query = normalize_hashtag(&params.q);
    let hashtags = if query.is_empty() {
        Vec::new()
    } else {
        let mut tx = state.db_pool.begin().await?;
        let hashtags = search_hashtags(&mut tx, &query, HASHTAG_AUTOCOMPLETE_LIMIT).await?;
        tx.commit().await?;
        hashtags
    };

    let rendered = state
        .env
        .get_template("hashtag_autocomplete.jinja")?
        .render(context! { hashtags, ftl_lang })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct HashtagDiscoveryQuery {
    q: Option<String>,
    sort: Option<String>,
}

/// The tags a directory request asks for, and the search it is answering.
///
/// The page and the search box reach this by different routes and have to agree
/// about what a given URL means: an empty box is browsing, not a search for the
/// empty string, on both. It used to be browsing on one and a `LIKE '%'` dump
/// titled `Search results for ""` on the other.
async fn requested_hashtags(
    state: &AppState,
    params: &HashtagDiscoveryQuery,
) -> Result<(Vec<Hashtag>, Option<String>, HashtagSort), AppError> {
    let sort = HashtagSort::from_param(params.sort.as_deref());
    let query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);

    let mut tx = state.db_pool.begin().await?;
    let hashtags = match query.as_deref() {
        Some(query) => search_hashtags(&mut tx, query, HASHTAG_LIST_LIMIT).await?,
        None => browse_hashtags(&mut tx, sort, HASHTAG_LIST_LIMIT).await?,
    };
    tx.commit().await?;

    Ok((hashtags, query, sort))
}

/// GET /api/hashtags/cards — the tag list alone, for the search box.
///
/// Shares `hashtag_results.jinja` with the page, and reaches the same queries
/// `hashtag_discovery` does, so typing into the box and loading the URL cannot
/// disagree about what matches. The page still answers the plain form GET for
/// anyone without scripting.
pub async fn hashtag_cards(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<HashtagDiscoveryQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (hashtags, search_query, _) = requested_hashtags(&state, &params).await?;

    let rendered = state
        .env
        .get_template("hashtag_results.jinja")?
        .render(context! {
            hashtags,
            search_query,
            ftl_lang,
        })?;

    Ok(Html(rendered).into_response())
}

/// GET /hashtags — the tag directory.
pub async fn hashtag_discovery(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<HashtagDiscoveryQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (hashtags, search_query, sort) = requested_hashtags(&state, &params).await?;

    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("hashtag_discovery.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        hashtags,
        search_query,
        sort_by => sort.as_param(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}
