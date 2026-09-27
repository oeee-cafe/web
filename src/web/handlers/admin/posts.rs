//! Every post on the site, whatever its visibility, and flagging them.

use crate::app_error::AppError;
use crate::models::admin::{
    count_all_posts, find_all_communities, find_all_posts, find_community_by_slug, find_post_by_id,
    set_post_explicit, AdminCommunity, AdminPostFilter,
};
use crate::models::handle::LoginName;
use crate::models::user::find_user_by_login_name;
use crate::web::context::CommonContext;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use axum::Form;
use minijinja::context;
use serde::Deserialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const POSTS_PER_PAGE: i64 = 60;

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
    author_login_name: Option<LoginName>,
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

    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let rendered = state
        .render_page(
            "admin/posts.jinja",
            common_ctx,
            context! {
                posts => posts,
                communities => communities,
                total => total,
                has_more => has_more,
                next_url => next_url,
                filter_author => resolved.author_login_name,
                filter_community => resolved.community_slug,
                include_drafts => resolved.filter.include_drafts,
                include_deleted => resolved.filter.include_deleted,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

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

    let rendered = state
        .render(
            "admin/posts_fragment.jinja",
            context! {
                posts => posts,
                has_more => has_more,
                next_url => next_url,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

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

    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let rendered = state
        .render_page(
            "admin/post_detail.jinja",
            common_ctx,
            context! {
                post => post,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

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

    let rendered = state
        .render("admin/post_flag_panel.jinja", context! { post => post })
        .await?;

    Ok(Html(rendered))
}
