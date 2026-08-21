//! Staff-only views. Every handler here takes `AdminUser`, which is what makes
//! the unfiltered queries in `models::admin` safe to expose.
//!
//! Copy is intentionally untranslated: this surface is staff-only, so it is not
//! worth carrying through the four locale bundles.

use crate::app_error::AppError;
use crate::models::admin::{
    count_all_posts, find_all_banners, find_all_collaborative_sessions, find_all_communities,
    find_all_communities_with_activity, find_all_posts, find_all_users, find_banner_by_id,
    find_community_by_slug, find_post_by_id, set_banner_explicit, set_post_explicit,
    AdminCommunity, AdminPostFilter, AdminSessionStatus, AdminSort,
};
use crate::web::handlers::collaborate::preview::preview_versions;
use crate::models::user::find_user_by_login_name;
use crate::web::context::CommonContext;
use crate::web::handlers::{AdminUser, ExtractFtlLang};
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use minijinja::context;
use serde::Deserialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const POSTS_PER_PAGE: i64 = 60;
const USERS_PER_PAGE: i64 = 100;
const BANNERS_PER_PAGE: i64 = 60;
const SESSIONS_PER_PAGE: i64 = 100;

/// Filters for the global post list. `author` and `community` are the
/// human-readable identifiers so the URLs stay hand-editable.
///
/// `drafts` / `deleted` are `Option<String>` rather than `bool` because HTML
/// checkboxes submit `drafts=on` when ticked and omit the key entirely when
/// not; presence is the signal.
#[derive(Debug, Default, Deserialize)]
pub struct AdminPostsQuery {
    pub author: Option<String>,
    pub community: Option<String>,
    pub drafts: Option<String>,
    pub deleted: Option<String>,
    /// Row offset for the infinite-scroll sentinel. The first page omits it.
    pub offset: Option<i64>,
}

struct ResolvedFilter {
    filter: AdminPostFilter,
    author_login_name: Option<String>,
    community_slug: Option<String>,
}

/// Turns login_name/slug into ids. An unknown name yields a filter that matches
/// nothing rather than silently widening the list to every post.
async fn resolve_filter(
    tx: &mut Transaction<'_, Postgres>,
    query: &AdminPostsQuery,
) -> Result<ResolvedFilter, AppError> {
    let mut filter = AdminPostFilter {
        include_drafts: query.drafts.is_some(),
        include_deleted: query.deleted.is_some(),
        ..Default::default()
    };

    let mut author_login_name = None;
    if let Some(login_name) = query.author.as_deref().filter(|s| !s.is_empty()) {
        let user = find_user_by_login_name(tx, login_name)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("User @{}", login_name)))?;
        filter.author_id = Some(user.id);
        author_login_name = Some(user.login_name);
    }

    let mut community_slug = None;
    if let Some(slug) = query.community.as_deref().filter(|s| !s.is_empty()) {
        let community = find_community_by_slug(tx, slug)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Community @{}", slug)))?;
        filter.community_id = Some(community.id);
        community_slug = Some(community.slug);
    }

    Ok(ResolvedFilter {
        filter,
        author_login_name,
        community_slug,
    })
}

/// URL the infinite-scroll sentinel fetches next. Built here rather than in the
/// template so author names and slugs get percent-encoded properly.
fn fragment_url(resolved: &ResolvedFilter, next_offset: i64) -> String {
    let mut url = format!("/admin/posts-fragment?offset={}", next_offset);
    if let Some(author) = &resolved.author_login_name {
        url.push_str(&format!("&author={}", urlencoding::encode(author)));
    }
    if let Some(slug) = &resolved.community_slug {
        url.push_str(&format!("&community={}", urlencoding::encode(slug)));
    }
    if resolved.filter.include_drafts {
        url.push_str("&drafts=on");
    }
    if resolved.filter.include_deleted {
        url.push_str("&deleted=on");
    }
    url
}

/// Loads one batch and builds the context the fragment template needs. Shared
/// by the full page and the infinite-scroll fragment so both stay in step.
async fn load_batch(
    tx: &mut Transaction<'_, Postgres>,
    query: &AdminPostsQuery,
    resolved: &ResolvedFilter,
) -> Result<(Vec<crate::models::admin::AdminPost>, bool, String), AppError> {
    let offset = query.offset.unwrap_or(0).max(0);
    let posts = find_all_posts(tx, resolved.filter, POSTS_PER_PAGE, offset).await?;
    // A full batch means there is probably more; a short one is definitively
    // the end. Costs one wasted request at an exact multiple, which beats
    // counting on every scroll.
    let has_more = posts.len() as i64 == POSTS_PER_PAGE;
    let next_url = fragment_url(resolved, offset + POSTS_PER_PAGE);
    Ok((posts, has_more, next_url))
}

/// Shared renderer for the global list and its pre-filtered variants.
async fn render_posts(
    admin: AdminUser,
    state: &AppState,
    ftl_lang: String,
    query: AdminPostsQuery,
    resolved: ResolvedFilter,
    communities: Vec<AdminCommunity>,
    mut tx: Transaction<'_, Postgres>,
) -> Result<Html<String>, AppError> {
    let (posts, has_more, next_url) = load_batch(&mut tx, &query, &resolved).await?;
    let total = count_all_posts(&mut tx, resolved.filter).await?;

    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/posts.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        posts => posts,
        communities => communities,
        total => total,
        has_more => has_more,
        next_url => next_url,
        filter_author => resolved.author_login_name,
        filter_community => resolved.community_slug,
        include_drafts => resolved.filter.include_drafts,
        include_deleted => resolved.filter.include_deleted,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

/// GET /admin/posts-fragment — one batch of cards plus the next sentinel, for
/// htmx to swap in. Distinct path rather than `/admin/posts/fragment` so it
/// cannot be confused with the `:post_id` detail route.
pub async fn admin_posts_fragment(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<AdminPostsQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let resolved = resolve_filter(&mut tx, &query).await?;
    let (posts, has_more, next_url) = load_batch(&mut tx, &query, &resolved).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/posts_fragment.jinja")?;
    let rendered = template.render(context! {
        posts => posts,
        has_more => has_more,
        next_url => next_url,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;

    Ok(Html(rendered))
}

/// GET /admin/posts — every post on the instance, across all users and
/// communities, with optional filters.
pub async fn admin_posts(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminPostsQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let resolved = resolve_filter(&mut tx, &query).await?;
    let communities = find_all_communities(&mut tx).await?;
    render_posts(admin, &state, ftl_lang, query, resolved, communities, tx).await
}

/// GET /admin/users/:login_name/posts — the global list scoped to one author,
/// including their unpublished drafts.
pub async fn admin_user_posts(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Query(query): Query<AdminPostsQuery>,
) -> Result<Html<String>, AppError> {
    let query = AdminPostsQuery {
        author: Some(login_name),
        // A per-author view that hid their drafts and removed posts would defeat
        // the point of opening it.
        drafts: query.drafts.or_else(|| Some("on".to_string())),
        deleted: query.deleted.or_else(|| Some("on".to_string())),
        ..query
    };

    let mut tx = state.db_pool.begin().await?;
    let resolved = resolve_filter(&mut tx, &query).await?;
    let communities = find_all_communities(&mut tx).await?;
    render_posts(admin, &state, ftl_lang, query, resolved, communities, tx).await
}

/// GET /admin/communities/:slug/posts — scoped to one community regardless of
/// its visibility or the admin's membership.
pub async fn admin_community_posts(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<AdminPostsQuery>,
) -> Result<Html<String>, AppError> {
    let query = AdminPostsQuery {
        community: Some(slug),
        drafts: query.drafts.or_else(|| Some("on".to_string())),
        deleted: query.deleted.or_else(|| Some("on".to_string())),
        ..query
    };

    let mut tx = state.db_pool.begin().await?;
    let resolved = resolve_filter(&mut tx, &query).await?;
    let communities = find_all_communities(&mut tx).await?;
    render_posts(admin, &state, ftl_lang, query, resolved, communities, tx).await
}

/// GET /admin/posts/:post_id — full detail for one post, soft-deleted included.
pub async fn admin_post_detail(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;

    let post = find_post_by_id(&mut tx, post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/post_detail.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        post => post,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct FlagPostForm {
    /// Desired end state, not a toggle, so a double-submit is idempotent.
    pub is_explicit: bool,
}

/// POST /admin/posts/:post_id/explicit — flag or unflag a post as explicit.
/// A flagged post is withheld and blurred exactly as if the author had ticked
/// sensitive, and the author cannot clear it by editing the post.
pub async fn admin_flag_post(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Form(form): Form<FlagPostForm>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    set_post_explicit(&mut tx, post_id, form.is_explicit, admin.0.id).await?;

    // Re-read so the panel reflects what actually landed, including flagged_at.
    let post = find_post_by_id(&mut tx, post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;
    tx.commit().await?;

    let template = state.env.get_template("admin/post_flag_panel.jinja")?;
    let rendered = template.render(context! { post => post })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct AdminBannersQuery {
    /// Row offset for the infinite-scroll sentinel. The first page omits it.
    pub offset: Option<i64>,
    /// `?explicit=on` narrows to already-flagged banners, for reviewing past
    /// decisions.
    pub explicit: Option<String>,
}

/// Loads one batch of banners plus the sentinel URL for the next. Shared by the
/// full page and the fragment so both stay in step.
async fn load_banner_batch(
    tx: &mut Transaction<'_, Postgres>,
    query: &AdminBannersQuery,
) -> Result<(Vec<crate::models::admin::AdminBanner>, bool, String), AppError> {
    let offset = query.offset.unwrap_or(0).max(0);
    let only_explicit = query.explicit.is_some();

    let banners = find_all_banners(tx, only_explicit, BANNERS_PER_PAGE, offset).await?;
    let has_more = banners.len() as i64 == BANNERS_PER_PAGE;

    let mut next_url = format!(
        "/admin/banners-fragment?offset={}",
        offset + BANNERS_PER_PAGE
    );
    if only_explicit {
        next_url.push_str("&explicit=on");
    }

    Ok((banners, has_more, next_url))
}

/// GET /admin/banners — banner review queue. Flagged banners are withheld from
/// the public /about page.
pub async fn admin_banners(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminBannersQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let (banners, has_more, next_url) = load_banner_batch(&mut tx, &query).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/banners.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        banners => banners,
        only_explicit => query.explicit.is_some(),
        has_more => has_more,
        next_url => next_url,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

/// GET /admin/banners-fragment — one batch of banner cards plus the next
/// sentinel, for htmx to swap in.
pub async fn admin_banners_fragment(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<AdminBannersQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let (banners, has_more, next_url) = load_banner_batch(&mut tx, &query).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/banners_fragment.jinja")?;
    let rendered = template.render(context! {
        banners => banners,
        has_more => has_more,
        next_url => next_url,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct FlagBannerForm {
    /// Desired end state, not a toggle, so a double-submit is idempotent.
    pub is_explicit: bool,
}

/// POST /admin/banners/:banner_id/explicit — flag or unflag a banner. Returns
/// the replacement card for htmx to swap in place.
pub async fn admin_flag_banner(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
    Form(form): Form<FlagBannerForm>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    set_banner_explicit(&mut tx, banner_id, form.is_explicit, admin.0.id).await?;

    // Re-read so the card reflects what actually landed, including flagged_at.
    let banner = find_banner_by_id(&mut tx, banner_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Banner".to_string()))?;
    tx.commit().await?;

    let template = state.env.get_template("admin/banner_card.jinja")?;
    let rendered = template.render(context! {
        banner => banner,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub page: Option<i64>,
    /// Missing or unrecognised sorts fall back to last-active.
    #[serde(default)]
    pub sort: AdminSort,
}

/// GET /admin/users — every account, deleted ones included. Sorted by last
/// activity by default.
pub async fn admin_users(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminListQuery>,
) -> Result<Html<String>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * USERS_PER_PAGE;

    let mut tx = state.db_pool.begin().await?;
    let users = find_all_users(&mut tx, query.sort, USERS_PER_PAGE, offset).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    let has_next = users.len() as i64 == USERS_PER_PAGE;
    let template = state.env.get_template("admin/users.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        users => users,
        page => page,
        sort => query.sort,
        has_next => has_next,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

/// GET /admin/communities — every community, private and unlisted included.
/// Sorted by last activity by default.
pub async fn admin_communities(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminListQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let communities = find_all_communities_with_activity(&mut tx, query.sort).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    let template = state.env.get_template("admin/communities.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        communities => communities,
        sort => query.sort,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct AdminSessionsQuery {
    pub page: Option<i64>,
    #[serde(default)]
    pub sort: AdminSort,
    /// Omit to show everything. A value that is not one of the three is a 400
    /// rather than a fallback -- `serde(default)` covers a missing key, not an
    /// unknown one -- which is the same deal `sort` above has offered since it
    /// was written.
    #[serde(default)]
    pub status: AdminSessionStatus,
}

/// GET /admin/collaborative-sessions — every session, link-only and
/// private-community ones included, live or ended.
///
/// The lobby can only ever show a person their own sessions and the public
/// ones, which leaves no way at all to see a room that is filling up out of
/// sight. Each live one carries the same participant-rendered preview the
/// lobby cards use, so what is being drawn is visible without joining and
/// taking a seat.
pub async fn admin_collaborative_sessions(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminSessionsQuery>,
) -> Result<Html<String>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * SESSIONS_PER_PAGE;

    let mut tx = state.db_pool.begin().await?;
    let mut sessions = find_all_collaborative_sessions(
        &mut tx,
        query.sort,
        query.status,
        SESSIONS_PER_PAGE,
        offset,
    )
    .await?;
    let common_ctx = CommonContext::build(&mut tx, Some(admin.0.id)).await?;
    tx.commit().await?;

    // After the transaction, never inside it: this is a round trip to Redis,
    // and holding a database connection across it would be paying for one pool
    // out of another.
    let room_uuids: Vec<Uuid> = sessions.iter().map(|session| session.id).collect();
    for (session, version) in sessions
        .iter_mut()
        .zip(preview_versions(&state, &room_uuids).await)
    {
        session.preview_version = version;
    }

    let has_next = sessions.len() as i64 == SESSIONS_PER_PAGE;
    let template = state
        .env
        .get_template("admin/collaborative_sessions.jinja")?;
    let rendered = template.render(context! {
        current_user => admin.0,
        sessions => sessions,
        page => page,
        sort => query.sort,
        status => query.status,
        has_next => has_next,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

/// GET /admin/collaborative-sessions/:uuid/archive — the whole recording of
/// one session, as one file.
///
/// The point of holding it is to run it back through the client and watch
/// where the canvas goes wrong, which is what a post-mortem of a desync
/// actually needs -- reconstructing one out of whatever survived in Redis an
/// hour later is how the last one had to be done.
pub async fn download_collaborative_archive(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Response, AppError> {
    let archive = crate::web::handlers::collaborate::archive::download_session(&state, room_uuid)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read the archive: {}", e))?;
    if archive.is_empty() {
        return Err(AppError::NotFound("No archive for this session".to_string()));
    }

    // A recording is mostly its own framing and goes over the wire at about a
    // third of its size. Compressed here rather than passed through from
    // storage: a session is kept as one gzip member per chunk, and
    // concatenating members is legal but leans on every consumer handling a
    // multi-member stream. Since this path already has to inflate each chunk
    // to join them, one deflate on the way out removes the question, and the
    // measured difference between one member and several is nothing.
    //
    // Content-Encoding rather than a `.gz` body, so what a reader decodes is
    // the recording either way -- the browser inflates before the player sees
    // it, and a person following the link gets the plain file.
    let wants_gzip = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("gzip"));

    let compressed = wants_gzip
        .then(|| crate::web::handlers::collaborate::archive::compress(&archive))
        .transpose()
        // Not worth failing a download over; it is only smaller.
        .unwrap_or_else(|e| {
            tracing::warn!("Could not compress the archive for {}: {}", room_uuid, e);
            None
        });

    let mut response = axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{room_uuid}.oeeelog\""),
        );
    if compressed.is_some() {
        response = response.header(header::CONTENT_ENCODING, "gzip");
    }
    Ok(response
        .body(axum::body::Body::from(compressed.unwrap_or(archive)))
        .map_err(|e| anyhow::anyhow!("Failed to build the archive response: {}", e))?)
}

/// GET /admin/collaborative-sessions/:uuid/diagnostics — what each client
/// believed about its own position, for the moments one of them said
/// something was wrong.
///
/// Staff only, like the recording it belongs to: a report carries one
/// person's session in enough detail to reconstruct it.
pub async fn download_collaborative_diagnostics(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let reports =
        crate::web::handlers::collaborate::archive::download_diagnostics(&state, room_uuid)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read the reports: {}", e))?;
    Ok(axum::Json(reports).into_response())
}

/// GET /admin/collaborative-sessions/:uuid/manifest — what a recording says
/// about itself, for the player to size a canvas and name the layers by.
pub async fn collaborative_archive_manifest(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let manifest = crate::web::handlers::collaborate::archive::read_manifest(&state, room_uuid)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read the manifest: {}", e))?;
    match manifest {
        Some(manifest) => Ok(axum::Json(manifest).into_response()),
        None => Err(AppError::NotFound("No recording for this session".to_string())),
    }
}

/// GET /admin/collaborative-sessions/:uuid/chat — what was said in a session,
/// beside the recording of what was drawn.
///
/// Chat never enters canonical history: it is broadcast and forgotten, with
/// only the last hundred lines held for somebody joining. This is the kept
/// copy, and staff-only like everything else about a recording -- a
/// transcript is the most personal thing a session produces.
pub async fn collaborative_session_chat(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let lines = crate::web::handlers::collaborate::archive::read_chat(&state, room_uuid)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read the transcript: {}", e))?;
    Ok(axum::Json(lines).into_response())
}

/// GET /admin/collaborative-sessions/:uuid/replay — the recording, played
/// back through the painter that drew it.
///
/// The page holds no permission of its own: it fetches the manifest and the
/// log from the two admin endpoints above, so serving it to anyone else would
/// get them a viewer that can read nothing.
pub async fn replay_collaborative_session(
    _admin: AdminUser,
    Path(_room_uuid): Path<Uuid>,
) -> Result<Response, AppError> {
    let html = std::fs::read_to_string("neo-cucumber/dist-replay/index.html")
        .map_err(|_| anyhow::anyhow!("The replay viewer has not been built"))?;
    Ok(Html(html).into_response())
}

#[cfg(test)]
mod tests {
    //! `cargo check` validates the handlers but not the Jinja, so render every
    //! admin template against representative context. Catches syntax errors,
    //! broken inheritance and unknown filters.

    use minijinja::{context, Environment};
    use serde_json::json;

    fn test_env() -> Environment<'static> {
        crate::web::handlers::test_support::env()
    }

    fn sample_post() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "title": "A drawing",
            "is_sensitive": false,
            "viewer_count": 12,
            "author_id": "00000000-0000-0000-0000-000000000002",
            "author_login_name": "someone",
            "author_display_name": "Some One",
            "community_id": "00000000-0000-0000-0000-000000000003",
            "community_slug": "secret",
            "community_name": "Secret Club",
            "community_visibility": "private",
            "image_filename": "abcdef.png",
            "image_width": 300,
            "image_height": 300,
            "published_at": "2026-01-02T03:04:05Z",
            "created_at": "2026-01-01T03:04:05Z",
            "deleted_at": "2026-02-01T03:04:05Z",
            "deletion_reason": "Moderation",
            "is_sensitive_by_author": false,
            "is_explicit": true,
            "explicit_flagged_at": "2026-06-01T00:00:00Z",
            "explicit_flagged_by_login_name": "admin",
        })
    }

    fn sample_communities() -> serde_json::Value {
        json!([{
            "id": "00000000-0000-0000-0000-000000000003",
            "slug": "secret",
            "name": "Secret Club",
            "visibility": "private",
            "created_at": "2026-01-01T00:00:00Z",
            "deleted_at": null,
            "post_count": 7,
            "last_active_at": "2026-03-01T00:00:00Z",
        }])
    }

    fn current_user() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000009",
            "login_name": "admin",
            "display_name": "Admin",
            "email_verified_at": "2026-01-01T00:00:00Z",
            "role": "admin",
        })
    }

    #[test]
    fn renders_posts_list() {
        let env = test_env();
        let template = env.get_template("admin/posts.jinja").expect("template loads");
        let rendered = template
            .render(context! {
                current_user => current_user(),
                posts => vec![sample_post()],
                communities => sample_communities(),
                total => 1,
                has_more => true,
                next_url => "/admin/posts-fragment?offset=60",
                filter_author => "someone",
                filter_community => "secret",
                include_drafts => true,
                include_deleted => true,
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("posts.jinja renders");
        // The column control drives --admin-cols; the grid must opt in via
        // `adjustable` or the slider changes nothing.
        assert!(rendered.contains("admin-grid adjustable"));
        assert!(rendered.contains("id=\"admin-cols\""));
        // The nav bar is full-bleed with its contents in the page column, the
        // same as every other page — admin no longer widens the header to match
        // its own full-bleed grid.
        assert!(rendered.contains("<nav class=\"nav-bar\">"));
        assert!(rendered.contains("id=\"menubar\" class=\"center\""));
    }

    #[test]
    fn renders_posts_list_when_empty() {
        let env = test_env();
        let template = env.get_template("admin/posts.jinja").expect("template loads");
        let rendered = template
            .render(context! {
                current_user => current_user(),
                posts => Vec::<serde_json::Value>::new(),
                communities => sample_communities(),
                total => 0,
                has_more => false,
                next_url => "/admin/posts-fragment?offset=60",
                filter_author => None::<String>,
                filter_community => None::<String>,
                include_drafts => false,
                include_deleted => false,
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("posts.jinja renders with no results");
        assert!(rendered.contains("No posts match this filter."));
    }

    #[test]
    fn post_titles_are_html_escaped() {
        // Regression: a real post titled `><//` unbalanced the card markup
        // because .jinja templates were not autoescaped, and a title carrying a
        // quote could break out of the alt attribute entirely.
        let env = test_env();
        let template = env
            .get_template("admin/posts_fragment.jinja")
            .expect("template loads");
        let mut post = sample_post();
        post["title"] = json!(r#"><//" onerror="alert(1)"#);
        let rendered = template
            .render(context! {
                posts => vec![post],
                has_more => false,
                next_url => "",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");

        // The raw title must not survive anywhere in the output.
        assert!(!rendered.contains(r#"><//" onerror="#));
        assert!(!rendered.contains("onerror=\"alert"));
        // ...and the div holding it must still close, so cards do not nest.
        assert_eq!(rendered.matches("<div").count(), rendered.matches("</div>").count());
    }

    #[test]
    fn shared_card_switches_targets_for_admin() {
        // The public feed and the admin grid render the same macro; `admin`
        // decides the link targets and the moderation tags. If that flag stops
        // working, admin cards quietly start linking readers into /admin.
        let env = test_env();
        let template = env
            .get_template("admin/posts_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                posts => vec![sample_post()],
                has_more => false,
                next_url => "",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");

        // Admin targets, not public ones.
        assert!(rendered.contains("/admin/posts/"));
        assert!(rendered.contains("/admin/users/someone/posts"));
        assert!(rendered.contains("/admin/communities/secret/posts"));
        assert!(!rendered.contains("/communities/@secret"));
        // Moderation tags are admin-only.
        assert!(rendered.contains("admin-tag"));
        assert!(rendered.contains("explicit"));
        // The handle fallback resolves author_login_name for admin rows.
        assert!(rendered.contains("@someone"));
        // Staff see sensitive content unblurred.
        assert!(!rendered.contains("class=\"sensitive\""));
        // ...and the shared attribution block is present either way.
        assert!(rendered.contains("post-card-byline"));
    }

    #[test]
    fn renders_posts_fragment_standalone() {
        // The fragment handler passes a strictly smaller context than the full
        // page, so render it with only those keys.
        let env = test_env();
        let template = env
            .get_template("admin/posts_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                posts => vec![sample_post()],
                has_more => true,
                next_url => "/admin/posts-fragment?offset=60&author=some%20one",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("posts_fragment.jinja renders standalone");
        assert!(rendered.contains("hx-trigger=\"revealed\""));
        // `&` is entity-encoded in the attribute now that escaping is on. That
        // is correct HTML: the parser decodes it, so getAttribute() hands htmx
        // back a plain `&`. Pinned so double-escaping would be caught.
        // Escaping encodes `/` and `&` as entities. Harmless in an attribute —
        // the HTML parser decodes them, so getAttribute() hands htmx back the
        // plain URL. Pinned so double-escaping would be caught.
        assert!(rendered
            .contains("&#x2f;admin&#x2f;posts-fragment?offset=60&amp;author=some%20one"));
    }

    #[test]
    fn fragment_omits_sentinel_on_last_batch() {
        let env = test_env();
        let template = env
            .get_template("admin/posts_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                posts => vec![sample_post()],
                has_more => false,
                next_url => "",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("posts_fragment.jinja renders");
        assert!(!rendered.contains("hx-trigger"));
    }

    #[test]
    fn renders_post_detail() {
        let env = test_env();
        let template = env
            .get_template("admin/post_detail.jinja")
            .expect("template loads");
        template
            .render(context! {
                current_user => current_user(),
                post => sample_post(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("post_detail.jinja renders");
    }

    fn sample_banner(is_explicit: bool) -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-00000000000b",
            "author_id": "00000000-0000-0000-0000-000000000002",
            "author_login_name": "someone",
            "image_filename": "bannerfile.png",
            "image_width": 300,
            "image_height": 100,
            "is_explicit": is_explicit,
            "flagged_at": if is_explicit { Some("2026-05-01T00:00:00Z") } else { None },
            "flagged_by_login_name": if is_explicit { Some("admin") } else { None },
            "is_active": true,
            "created_at": "2026-01-01T00:00:00Z",
        })
    }

    #[test]
    fn post_flag_panel_renders_standalone_for_htmx_swap() {
        let env = test_env();
        let template = env
            .get_template("admin/post_flag_panel.jinja")
            .expect("template loads");

        let rendered = template
            .render(context! { post => sample_post() })
            .expect("flagged panel renders");
        assert!(rendered.contains("flagged explicit by staff"));
        assert!(rendered.contains("Remove explicit flag"));
        // Flipping back must post the opposite state, not a blind toggle.
        assert!(rendered.contains("value=\"false\""));

        let mut unflagged = sample_post();
        unflagged["is_explicit"] = json!(false);
        unflagged["explicit_flagged_at"] = json!(null);
        unflagged["explicit_flagged_by_login_name"] = json!(null);
        let rendered = template
            .render(context! { post => unflagged })
            .expect("unflagged panel renders");
        assert!(rendered.contains("Flag as explicit"));
        assert!(rendered.contains("value=\"true\""));
    }

    #[test]
    fn renders_banner_queue() {
        let env = test_env();
        let template = env
            .get_template("admin/banners.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                current_user => current_user(),
                banners => vec![sample_banner(false), sample_banner(true)],
                only_explicit => false,
                has_more => true,
                next_url => "/admin/banners-fragment?offset=60",
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("banners.jinja renders");
        assert!(rendered.contains("Flag as explicit"));
        assert!(rendered.contains("Unflag"));
    }

    #[test]
    fn renders_banners_fragment_standalone() {
        let env = test_env();
        let template = env
            .get_template("admin/banners_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                banners => vec![sample_banner(false)],
                has_more => true,
                next_url => "/admin/banners-fragment?offset=60&explicit=on",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("banners_fragment.jinja renders standalone");
        assert!(rendered.contains("hx-trigger=\"revealed\""));
        assert!(rendered
            .contains("&#x2f;admin&#x2f;banners-fragment?offset=60&amp;explicit=on"));
        // The nested card must still render inside the fragment.
        assert!(rendered.contains("Flag as explicit"));
    }

    #[test]
    fn banner_card_renders_standalone_for_htmx_swap() {
        // The flag endpoint returns this template alone, so it must not depend
        // on anything the queue page supplies.
        let env = test_env();
        let template = env
            .get_template("admin/banner_card.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                banner => sample_banner(true),
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("banner_card.jinja renders standalone");
        assert!(rendered.contains("hidden from /about"));
        // Flipping back must post the opposite state, not a blind toggle.
        assert!(rendered.contains("value=\"false\""));
    }

    /// A session card's admin context. Defaults describe a live, link-only,
    /// personal session with a preview; tests override the field under test.
    fn sample_session(overrides: serde_json::Value) -> serde_json::Value {
        let mut session = json!({
            "id": "00000000-0000-0000-0000-000000000009",
            "title": "Doodle",
            "owner_login_name": "someone",
            "width": 1024,
            "height": 768,
            "max_participants": 4,
            "active_participant_count": 2,
            "total_participant_count": 5,
            "is_public": false,
            "community_slug": null,
            "community_name": null,
            "community_visibility": null,
            "created_at": "2026-01-02T03:04:05",
            "last_activity": "2026-01-02T05:06:07",
            "ended_at": null,
            "saved_post_id": null,
            "preview_version": 1_700_000_000_123u64,
        });
        for (key, value) in overrides.as_object().expect("object") {
            session[key] = value.clone();
        }
        session
    }

    fn sessions_context(sessions: Vec<serde_json::Value>) -> minijinja::Value {
        context! {
            current_user => current_user(),
            sessions => sessions,
            page => 1,
            sort => "active",
            status => "all",
            has_next => false,
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    fn render_sessions(sessions: Vec<serde_json::Value>) -> String {
        let env = test_env();
        env.get_template("admin/collaborative_sessions.jinja")
            .expect("template loads")
            .render(sessions_context(sessions))
            .expect("collaborative_sessions.jinja renders")
    }

    /// The reason this page exists: a room the lobby will not list for
    /// anyone who is not already in it.
    #[test]
    fn session_list_marks_the_ones_the_lobby_hides() {
        let rendered = render_sessions(vec![
            sample_session(json!({})),
            sample_session(json!({
                "id": "00000000-0000-0000-0000-00000000000a",
                "is_public": true,
                "community_slug": "secret",
                "community_name": "Back Room",
                "community_visibility": "private",
            })),
        ]);
        assert!(rendered.contains("link only"));
        assert!(rendered.contains("admin-tag private"));
        assert!(rendered.contains("Back Room"));
    }

    /// The canvas itself, which is the thing an admin cannot otherwise see
    /// without taking a seat in the room.
    #[test]
    fn session_list_shows_the_live_canvas() {
        let rendered = render_sessions(vec![sample_session(json!({}))]);
        assert!(rendered.contains(
            "/collaborate/00000000-0000-0000-0000-000000000009/preview?v=1700000000123"
        ));
    }

    /// An ended session's Redis state is deleted with it, so there is nothing
    /// to point an `<img>` at and a row must not try.
    #[test]
    fn session_list_omits_the_canvas_once_a_session_is_over() {
        let rendered = render_sessions(vec![sample_session(json!({
            "ended_at": "2026-01-03T00:00:00Z",
            "preview_version": null,
            "saved_post_id": "00000000-0000-0000-0000-00000000000b",
        }))]);
        assert!(!rendered.contains("/preview?v="));
        // The canvas is a post by then, and that is where it is reachable.
        assert!(rendered.contains(
            "/@someone/00000000-0000-0000-0000-00000000000b"
        ));
        // Nor is there a live room left to open.
        assert!(!rendered.contains("/collaborate/00000000-0000-0000-0000-000000000009\""));
    }

    /// The recording is offered for every row, ended ones included -- an
    /// ended session is exactly the one worth examining, and the archive
    /// outlives the room it came from.
    #[test]
    fn session_rows_link_to_their_recording() {
        let rendered = render_sessions(vec![
            sample_session(json!({})),
            sample_session(json!({
                "id": "00000000-0000-0000-0000-00000000000c",
                "ended_at": "2026-01-03T00:00:00Z",
                "preview_version": null,
            })),
        ]);
        assert!(rendered.contains(
            "/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009/archive"
        ));
        assert!(rendered.contains(
            "/admin/collaborative-sessions/00000000-0000-0000-0000-00000000000c/archive"
        ));
        // And the reports filed against it, which are read together with the
        // stream they disagree with.
        assert!(rendered.contains(
            "/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009/diagnostics"
        ));
        // The recording is worth more played than read, so the viewer comes
        // first on the row.
        assert!(rendered.contains(
            "/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009/replay"
        ));
        // What was said, which is kept beside the recording and never entered
        // canonical history.
        assert!(rendered.contains(
            "/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009/chat"
        ));
    }

    /// Sorting and filtering have to survive each other: a status link that
    /// dropped the sort would silently reset the page under the admin.
    #[test]
    fn session_list_filters_keep_the_current_sort() {
        let rendered = render_sessions(vec![sample_session(json!({}))]);
        assert!(rendered.contains("?sort=active&status=live"));
        assert!(rendered.contains("?status=all&sort=created"));
    }

    #[test]
    fn renders_users_list() {
        let env = test_env();
        let template = env.get_template("admin/users.jinja").expect("template loads");
        template
            .render(context! {
                current_user => current_user(),
                users => json!([{
                    "id": "00000000-0000-0000-0000-000000000002",
                    "login_name": "someone",
                    "display_name": "Some One",
                    "role": "user",
                    "post_count": 4,
                    "created_at": "2026-01-01T00:00:00Z",
                    "deleted_at": null,
                    "last_active_at": "2026-04-01T00:00:00Z",
                }]),
                page => 1,
                sort => "active",
                has_next => false,
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("users.jinja renders");
    }

    #[test]
    fn renders_communities_list() {
        let env = test_env();
        let template = env
            .get_template("admin/communities.jinja")
            .expect("template loads");
        template
            .render(context! {
                current_user => current_user(),
                communities => sample_communities(),
                sort => "active",
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("communities.jinja renders");
    }
}
