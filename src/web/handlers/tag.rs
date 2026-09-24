use crate::app_error::AppError;
use crate::models::tag::{
    browse_tags, find_tag_by_name, find_posts_by_tag, tag_covers,
    normalize_tag, search_tags, Tag, TagCover, TagSort,
};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::models::comment::CommentScope;
use crate::web::handlers::home::{
    comments_batch, comments_context, feed_context, CommentsQuery, HOME_POSTS_PER_BATCH,
};
use crate::web::handlers::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use minijinja::context;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};

/// Tags listed on the directory, and matches returned by a search.
const TAG_LIST_LIMIT: i64 = 100;

/// Suggestions offered under the tag field while someone types.
const TAG_AUTOCOMPLETE_LIMIT: i64 = 10;

/// Where `/tags/:name` and its load-more endpoint agree on a tag.
///
/// The path segment is normalized the same way the tag field normalizes what
/// was typed into it, so `/tags/Art`, `/tags/art` and `/tags/#art`
/// are one page rather than three, one of which 404s.
enum Requested {
    /// The path already spells the tag the way it is stored.
    Canonical(String),
    /// It does not; send the reader to the spelling that does.
    Elsewhere(String),
}

fn canonicalize(requested: &str) -> Requested {
    let normalized = normalize_tag(requested);
    if normalized == requested {
        Requested::Canonical(normalized)
    } else {
        Requested::Elsewhere(normalized)
    }
}

/// `/tags/<name>`, with the name escaped. Tags are letters, digits and
/// underscores now, so this only ever has non-ASCII to encode — but it is the
/// difference between a link that works for 그림 and one that depends on the
/// browser guessing.
fn tag_url(name: &str) -> String {
    format!("/tags/{}", urlencoding::encode(name))
}

/// The 404 page, rather than the bare `<h1>Tag not found</h1>` string this
/// used to answer with: unstyled, untranslated, and outside the site chrome.
async fn tag_not_found(
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

/// Which half of a tag's page is showing: its drawings, or what has been
/// said on them. A pill apart, as a community's are (tag_view.jinja).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TagView {
    Drawings,
    Comments,
}

impl TagView {
    fn url(self, name: &str) -> String {
        match self {
            TagView::Drawings => tag_url(name),
            TagView::Comments => format!("{}/comments", tag_url(name)),
        }
    }
}

/// `/api/tags/<name>/comments`: where a tag's list of comments loads its
/// next batch from.
fn tag_comments_path(name: &str) -> String {
    format!("/api/tags/{}/comments", urlencoding::encode(name))
}

/// A tag's page: the card, then its drawings with the comments on them
/// beside, or those comments alone.
async fn tag_page(
    view: TagView,
    auth_session: AuthSession,
    state: AppState,
    ftl_lang: String,
    requested: String,
) -> Result<axum::response::Response, AppError> {
    let name = match canonicalize(&requested) {
        Requested::Canonical(name) => name,
        Requested::Elsewhere(name) if name.is_empty() => {
            let mut tx = state.db_pool.begin().await?;
            let response = tag_not_found(&mut tx, &state, &auth_session, &ftl_lang).await?;
            tx.commit().await?;
            return Ok(response);
        }
        Requested::Elsewhere(name) => {
            return Ok(Redirect::permanent(&view.url(&name)).into_response())
        }
    };

    let mut tx = state.db_pool.begin().await?;

    let Some(tag) = find_tag_by_name(&mut tx, &name).await? else {
        let response = tag_not_found(&mut tx, &state, &auth_session, &ftl_lang).await?;
        tx.commit().await?;
        return Ok(response);
    };

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    // The total comes back from the same query as the posts, over the same
    // filter, so the count in the heading is the number of drawings below it.
    // The comments half shows no drawings, but still counts them for the
    // card, and still has the latest for the link preview.
    let limit = match view {
        TagView::Drawings => HOME_POSTS_PER_BATCH,
        TagView::Comments => 1,
    };
    let (posts, post_count) = find_posts_by_tag(
        &mut tx,
        &name,
        limit,
        0,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    let comments =
        comments_batch(&mut tx, CommentScope::Tag(tag.id), auth_session.user.as_ref(), None)
            .await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    let template = state.env.get_template(match view {
        TagView::Drawings => "tag_view.jinja",
        TagView::Comments => "tag_comments.jinja",
    })?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        tag => tag,
        post_count,
        feed => feed_context(posts, &format!("{}/posts", tag_url(&name)), 0, None),
        comments => comments_context(comments, &tag_comments_path(&name)),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

/// GET /tags/:tag_name — one tag's drawings.
pub async fn tag_view(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(requested): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    tag_page(TagView::Drawings, auth_session, state, ftl_lang, requested).await
}

/// GET /tags/:tag_name/comments — what has been said on its drawings.
pub async fn tag_comments(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(requested): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    tag_page(TagView::Comments, auth_session, state, ftl_lang, requested).await
}

/// GET /api/tags/:tag_name/comments — the next batch of a tag's comments
/// and the sentinel for the one after.
pub async fn load_more_tag_comments(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(requested): Path<String>,
    Query(query): Query<CommentsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let name = normalize_tag(&requested);
    let mut tx = state.db_pool.begin().await?;
    let tag = find_tag_by_name(&mut tx, &name)
        .await?
        .ok_or_else(|| AppError::NotFound("Tag".to_string()))?;
    let comments = comments_batch(
        &mut tx,
        CommentScope::Tag(tag.id),
        auth_session.user.as_ref(),
        query.after,
    )
    .await?;
    tx.commit().await?;

    let rendered = state.env.get_template("comments_fragment.jinja")?.render(context! {
        comments => comments_context(comments, &tag_comments_path(&name)),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;
    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct LoadMoreQuery {
    offset: i64,
    limit: i64,
    /// The month the previous batch ended in (home::feed_context).
    period: Option<String>,
}

/// GET /tags/:tag_name/posts — the next batch of cards for the tag
/// page's infinite scroll. Same fragment every other feed loads.
pub async fn load_more_tag_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(requested): Path<String>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    let name = normalize_tag(&requested);

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let mut tx = state.db_pool.begin().await?;
    let (posts, _) = find_posts_by_tag(
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
            feed => feed_context(
                posts,
                &format!("{}/posts", tag_url(&name)),
                query.offset,
                query.period.as_deref(),
            ),
            r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct AutocompleteQuery {
    q: String,
}

/// GET /api/tags/autocomplete — suggestions under the tag field.
pub async fn tag_autocomplete(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<AutocompleteQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Nothing typed is nothing to suggest. The field fires on every keystroke
    // including the one that empties it, and an empty query used to match every
    // tag on the site and drop the menu open over the form.
    let query = normalize_tag(&params.q);
    let tags = if query.is_empty() {
        Vec::new()
    } else {
        let mut tx = state.db_pool.begin().await?;
        let tags = search_tags(&mut tx, &query, TAG_AUTOCOMPLETE_LIMIT).await?;
        tx.commit().await?;
        tags
    };

    let rendered = state
        .env
        .get_template("tag_autocomplete.jinja")?
        .render(context! { tags, ftl_lang })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct TagDiscoveryQuery {
    q: Option<String>,
    sort: Option<String>,
}

/// The tags a directory request asks for, and the search it is answering.
///
/// The page and the search box reach this by different routes and have to agree
/// about what a given URL means: an empty box is browsing, not a search for the
/// empty string, on both. It used to be browsing on one and a `LIKE '%'` dump
/// titled `Search results for ""` on the other.
/// A tag as the directory shows it: the tag, and the drawings on its card.
#[derive(Serialize)]
struct TagCard {
    #[serde(flatten)]
    tag: Tag,
    covers: Vec<TagCover>,
}

async fn requested_tags(
    state: &AppState,
    params: &TagDiscoveryQuery,
) -> Result<(Vec<TagCard>, Option<String>, TagSort), AppError> {
    let sort = TagSort::from_param(params.sort.as_deref());
    let query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);

    let mut tx = state.db_pool.begin().await?;
    let tags = match query.as_deref() {
        Some(query) => search_tags(&mut tx, query, TAG_LIST_LIMIT).await?,
        None => browse_tags(&mut tx, sort, TAG_LIST_LIMIT).await?,
    };
    let ids: Vec<_> = tags.iter().map(|h| h.id).collect();
    let mut covers = tag_covers(&mut tx, &ids).await?;
    tx.commit().await?;

    let cards = tags
        .into_iter()
        .map(|tag| {
            let (mine, rest): (Vec<_>, Vec<_>) =
                covers.drain(..).partition(|c| c.tag_id == tag.id);
            covers = rest;
            TagCard { tag, covers: mine }
        })
        .collect();

    Ok((cards, query, sort))
}

/// GET /api/tags/cards — the tag list alone, for the search box.
///
/// Shares `tag_results.jinja` with the page, and reaches the same queries
/// `tag_discovery` does, so typing into the box and loading the URL cannot
/// disagree about what matches. The page still answers the plain form GET for
/// anyone without scripting.
pub async fn tag_cards(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<TagDiscoveryQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (tags, search_query, _) = requested_tags(&state, &params).await?;

    let rendered = state
        .env
        .get_template("tag_results.jinja")?
        .render(context! {
            tags,
            search_query,
            ftl_lang,
        })?;

    Ok(Html(rendered).into_response())
}

/// GET /tags — the tag directory.
pub async fn tag_discovery(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(params): Query<TagDiscoveryQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (tags, search_query, sort) = requested_tags(&state, &params).await?;

    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("tag_discovery.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        tags,
        search_query,
        sort_by => sort.as_param(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}
