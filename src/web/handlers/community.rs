use crate::app_error::AppError;
use crate::models::actor::create_actor_for_community;
use crate::models::comment::CommentScope;
use crate::models::community::{
    accept_invitation, add_community_member, create_community, create_invitation,
    find_community_by_id, find_community_by_slug, get_communities_members_count,
    get_community_members_with_details, get_community_stats, get_invitation_by_id,
    get_own_communities, get_participating_communities,
    get_pending_invitations_with_invitee_details_for_community, get_public_communities,
    get_public_communities_paginated, get_user_role_in_community, is_user_member, leave_community,
    reject_invitation, remove_community_member, search_public_communities,
    slug_conflicts_with_user, soft_delete_community_with_activity, update_community_with_activity,
    Community, CommunityDraft, CommunityMemberRole, CommunitySort, CommunityVisibility,
};
use crate::models::notification::{
    format_community_invitation_message, get_user_language_preference,
};
use crate::models::post::{find_published_posts_by_community_id, find_recent_posts_by_communities};
use crate::models::user::{find_user_by_id, find_user_by_login_name, AuthSession};
use crate::web::handlers::home::{
    comments_batch, comments_context, feed_context, CommentsQuery, LoadMoreQuery,
    HOME_POSTS_PER_BATCH,
};
use crate::web::handlers::render_403;
use crate::web::handlers::{parse_id_with_legacy_support, ParsedId};
use crate::web::state::AppState;
use axum::extract::{Path, Query};
use axum::http::{uri::Uri, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, Form};
use axum_messages::Messages;
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use crate::web::context::CommonContext;
use crate::web::handlers::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};

pub async fn redirect_community_to_unified(Path(slug): Path<String>) -> Redirect {
    Redirect::permanent(&format!("/@{}", slug))
}

pub async fn community(
    auth_session: AuthSession,
    headers: HeaderMap,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::InvalidFormData("Invalid slug format".to_string()))?
            .to_string();
        find_community_by_slug(&mut tx, slug).await?
    } else {
        // Handle UUID format - redirect to @slug
        let uuid = match parse_id_with_legacy_support(&id, "/communities", &state)? {
            ParsedId::Uuid(uuid) => uuid,
            ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
            ParsedId::InvalidId(error_response) => return Ok(error_response),
        };
        let community = find_community_by_id(&mut tx, uuid).await?;
        if let Some(community) = &community {
            // Redirect UUID to @slug format
            return Ok(Redirect::to(&format!("/@{}", community.slug)).into_response());
        } else {
            None
        }
    };

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    render_community_page(
        &mut tx,
        &state,
        &auth_session,
        &headers,
        ftl_lang,
        community,
        uri.path(),
    )
    .await
}

/// Who keeps a community and how much has been drawn in it, for the header's
/// meta line. Every render of the header needs it -- the page, cancelling an
/// edit and saving one -- or the line would vanish after an edit.
async fn community_header_context(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community: &Community,
) -> Result<minijinja::Value, AppError> {
    let owner = find_user_by_id(tx, community.owner_id).await?;
    let stats = get_community_stats(tx, community.id).await?;
    Ok(context! {
        owner => owner.map(|u| context! {
            login_name => u.login_name,
            display_name => u.display_name,
        }),
        posts_count => stats.total_posts,
        contributors_count => stats.total_contributors,
    })
}

/// Renders the community page: header, drawing form and the community's own
/// post feed.
///
/// Shared by `/communities/:id` and the unified `/@:slug` route, which had a
/// copy each and were only kept in step by hand. They differ solely in how the
/// community was looked up, so everything past that point lives here.
pub(crate) async fn render_community_page(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    state: &AppState,
    auth_session: &AuthSession,
    headers: &HeaderMap,
    ftl_lang: String,
    community: Community,
    request_path: &str,
) -> Result<axum::response::Response, AppError> {
    let community_uuid = community.id;

    // Access control: verify access based on community visibility
    match community.visibility {
        CommunityVisibility::Private => {
            // Private communities require authentication AND membership
            match &auth_session.user {
                Some(user) => {
                    // User is authenticated, check membership
                    let is_member = is_user_member(tx, user.id, community_uuid).await?;
                    if !is_member {
                        // Authenticated but not a member - show 403 forbidden
                        return Ok(render_403(auth_session, state, ftl_lang)
                            .await?
                            .into_response());
                    }
                }
                None => {
                    // Not authenticated - redirect to login with next URL
                    return Ok(
                        Redirect::to(&format!("/login?next={}", request_path)).into_response()
                    );
                }
            }
        }
        CommunityVisibility::Public | CommunityVisibility::Unlisted => {
            // Public and unlisted communities are accessible to everyone
            // No authentication required
        }
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("community.jinja")?;

    // Cancelling the edit form swaps the header back in on its own; the feed
    // below it is untouched, so it is not worth a query. The template renders
    // its grid from whatever `feed` holds, and copes with it being absent.
    //
    // Only when the Cancel asks for it by name (`Oeee-Part: header`,
    // community_edit.jinja). Every other htmx request here wants the whole
    // page: the pill between the drawings and the comments boosts to this
    // address, and htmx restores history from it. Telling them apart by
    // `HX-Request-Type: full` was not enough, because htmx's preload fetches
    // a hovered pill before that header is set -- so hovering Drawings and
    // then clicking it swapped the header alone in for the whole content
    // area, and the page went blank below the toolbar.
    let header = community_header_context(tx, &community).await?;
    let wants_header_only = headers.get("HX-Request") == Some(&HeaderValue::from_static("true"))
        && headers.get("Oeee-Part") == Some(&HeaderValue::from_static("header"));
    if wants_header_only {
        let rendered = template
            .render_captured_to(
                context! {
                    current_user => auth_session.user,
                    community => Some(&community),
                    header => header,
                    community_id => community_uuid.to_string(),
                    domain => state.config.domain.clone(),
                    ftl_lang
                },
                std::io::sink(),
            )?
            .with_state_mut(|state| state.render_block("community_edit_block"))?;
        return Ok(Html(rendered).into_response());
    }

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    let posts = find_published_posts_by_community_id(
        tx,
        community_uuid,
        HOME_POSTS_PER_BATCH,
        0,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    let comments = comments_batch(
        tx,
        CommentScope::Community(community_uuid),
        auth_session.user.as_ref(),
        None,
    )
    .await?;
    let common_ctx = CommonContext::build(tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let rendered = template.render(context! {
        current_user => auth_session.user,
        community => Some(&community),
        header => header,
        community_id => community_uuid.to_string(),
        domain => state.config.domain.clone(),
        unread_notification_count => common_ctx.unread_notification_count,
        feed => feed_context(posts, &community_posts_path(&community.slug), 0, None),
        comments => comments_context(comments, &community_comments_path(&community.slug)),
        draft_post_count => common_ctx.draft_post_count,
        ftl_lang,
    })?;
    Ok(Html(rendered).into_response())
}

/// The load-more endpoint a community feed's sentinel points at. One function
/// so the route and the URL the page emits cannot disagree.
fn community_posts_path(slug: &str) -> String {
    format!("/api/communities/@{}/posts", slug)
}

/// The same, for the community's comments: beside its drawings and on its
/// comments page.
fn community_comments_path(slug: &str) -> String {
    format!("/api/communities/@{}/comments", slug)
}

/// GET /api/communities/@:slug/comments — the next batch of a community's
/// comments plus the sentinel for the one after. It repeats the pages'
/// visibility check, because the endpoint can be called on its own.
pub async fn load_more_community_comments(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<CommentsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let community = find_community_by_slug(&mut tx, slug)
        .await?
        .ok_or_else(|| AppError::NotFound("Community".to_string()))?;
    if community.visibility == CommunityVisibility::Private {
        let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
        if !is_user_member(&mut tx, user.id, community.id).await? {
            return Err(AppError::Forbidden);
        }
    }
    let comments = comments_batch(
        &mut tx,
        CommentScope::Community(community.id),
        auth_session.user.as_ref(),
        query.after,
    )
    .await?;
    tx.commit().await?;

    let rendered = state
        .env
        .get_template("comments_fragment.jinja")?
        .render(context! {
            comments => comments_context(comments, &community_comments_path(&community.slug)),
            r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            ftl_lang,
        })?;
    Ok(Html(rendered).into_response())
}

/// GET /api/communities/@:slug/posts — the next batch of a community's cards
/// plus the sentinel that pulls the batch after it. Same shape as the home
/// feed's loader, and it repeats the page's visibility check because the
/// endpoint can be called on its own.
pub async fn load_more_community_posts(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = find_community_by_slug(&mut tx, slug.clone())
        .await?
        .ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    if community.visibility == CommunityVisibility::Private {
        let user_id = viewer_user_id.ok_or(AppError::Unauthorized)?;
        if !is_user_member(&mut tx, user_id, community.id).await? {
            return Err(AppError::Forbidden);
        }
    }

    let posts = find_published_posts_by_community_id(
        &mut tx,
        community.id,
        query.limit,
        query.offset,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    tx.commit().await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("post_feed_fragment.jinja")?;
    let rendered = template.render(context! {
        feed => feed_context(
            posts,
            &community_posts_path(&community.slug),
            query.offset,
            query.period.as_deref(),
        ),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn community_iframe(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::InvalidFormData("Invalid slug format".to_string()))?
            .to_string();
        find_community_by_slug(&mut tx, slug).await?
    } else {
        // Handle UUID format - redirect to @slug
        let uuid = match parse_id_with_legacy_support(&id, "/communities", &state)? {
            ParsedId::Uuid(uuid) => uuid,
            ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
            ParsedId::InvalidId(error_response) => return Ok(error_response),
        };
        let community = find_community_by_id(&mut tx, uuid).await?;
        if let Some(community) = &community {
            // Redirect UUID to @slug format
            return Ok(
                Redirect::to(&format!("/communities/@{}/embed", community.slug)).into_response(),
            );
        } else {
            None
        }
    };

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;
    let community_uuid = community.id;

    // Access control: verify access based on community visibility
    match community.visibility {
        CommunityVisibility::Private => {
            // Private communities require authentication AND membership
            match &auth_session.user {
                Some(user) => {
                    // User is authenticated, check membership
                    let is_member = is_user_member(&mut tx, user.id, community_uuid).await?;
                    if !is_member {
                        // Authenticated but not a member - show 403 forbidden
                        return Ok(render_403(&auth_session, &state, ftl_lang)
                            .await?
                            .into_response());
                    }
                }
                None => {
                    // Not authenticated - redirect to login with next URL
                    let next_url = uri.path();
                    return Ok(Redirect::to(&format!("/login?next={}", next_url)).into_response());
                }
            }
        }
        CommunityVisibility::Public | CommunityVisibility::Unlisted => {
            // Public and unlisted communities are accessible to everyone
            // No authentication required
        }
    }

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    let posts = find_published_posts_by_community_id(
        &mut tx,
        community_uuid,
        1000,
        0,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("community_iframe.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        community => community,
        posts,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

/// Attaches the per-community extras the cards render: three recent posts and
/// the contributor count. Shared by the directory page and the infinite-scroll
/// fragment so a card looks the same however it arrived.
async fn enrich_public_communities(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    communities: &[crate::models::community::PublicCommunity],
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<serde_json::Value>, AppError> {
    if communities.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<Uuid> = communities.iter().map(|c| c.id).collect();
    let recent_posts =
        find_recent_posts_by_communities(tx, &ids, 3, viewer_user_id, viewer_show_sensitive)
            .await?;
    let members_stats = get_communities_members_count(tx, &ids).await?;

    let mut posts_by: std::collections::HashMap<Uuid, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    // The query hands back each community's posts newest first, so the first one
    // seen is the last time the community was active — the key the default sort
    // orders by, which the card had no way to show.
    let mut last_post_by: std::collections::HashMap<Uuid, chrono::DateTime<chrono::Utc>> =
        std::collections::HashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            if let Some(published_at) = post.published_at {
                last_post_by.entry(community_id).or_insert(published_at);
            }
            posts_by
                .entry(community_id)
                .or_default()
                .push(serde_json::json!({
                    "id": post.id.to_string(),
                    "image_filename": post.image_filename,
                    "image_width": post.image_width,
                    "image_height": post.image_height,
                    "author_login_name": post.author_login_name,
                }));
        }
    }

    let mut members_by: std::collections::HashMap<Uuid, Option<i64>> =
        std::collections::HashMap::new();
    for stat in members_stats {
        members_by.insert(stat.community_id, stat.members_count);
    }

    Ok(communities
        .iter()
        .map(|community| {
            serde_json::json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "description": community.description,
                "visibility": community.visibility,
                "owner_login_name": community.owner_login_name,
                "posts_count": community.posts_count,
                "members_count": members_by.get(&community.id).cloned().unwrap_or(None),
                "recent_posts": posts_by.get(&community.id).cloned().unwrap_or_default(),
                "last_post_at": last_post_by.get(&community.id),
            })
        })
        .collect())
}

/// Communities per batch in the public directory.
const COMMUNITIES_PER_BATCH: i64 = 20;

/// GET /api/communities/cards — one batch of public community cards plus the
/// next sentinel, for htmx to swap in.
pub async fn communities_fragment(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<CommunitiesQuery>,
) -> Result<Html<String>, AppError> {
    let offset = query.offset.unwrap_or(0).max(0);

    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let term = query.q.as_deref().map(str::trim).filter(|s| !s.is_empty());

    let mut tx = state.db_pool.begin().await?;
    let rows = match term {
        Some(term) => {
            search_public_communities(&mut tx, term, COMMUNITIES_PER_BATCH, offset).await?
        }
        None => {
            get_public_communities_paginated(&mut tx, query.sort, COMMUNITIES_PER_BATCH, offset)
                .await?
        }
    };
    let has_more = rows.len() as i64 == COMMUNITIES_PER_BATCH;
    let communities =
        enrich_public_communities(&mut tx, &rows, viewer_user_id, viewer_show_sensitive).await?;
    tx.commit().await?;

    let template = state.env.get_template("community_cards_fragment.jinja")?;
    let rendered = template.render(context! {
        communities => communities,
        has_more => has_more,
        next_url => communities_fragment_url(query.sort, term, offset + COMMUNITIES_PER_BATCH),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct CommunitiesQuery {
    /// Missing or unrecognised sorts fall back to last-active.
    #[serde(default)]
    pub sort: CommunitySort,
    /// Row offset for the infinite-scroll sentinel. The first page omits it.
    pub offset: Option<i64>,
    /// Search term. Server-side because the directory is paginated now — a
    /// client-side filter would only ever search the batches already loaded.
    pub q: Option<String>,
}

/// URL the infinite-scroll sentinel fetches next. Built in Rust so the search
/// term gets percent-encoded.
fn communities_fragment_url(sort: CommunitySort, q: Option<&str>, next_offset: i64) -> String {
    let mut url = format!(
        "/api/communities/cards?offset={}&sort={}",
        next_offset,
        sort.as_param()
    );
    if let Some(term) = q.filter(|s| !s.trim().is_empty()) {
        url.push_str(&format!("&q={}", urlencoding::encode(term)));
    }
    url
}

pub async fn communities(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<CommunitiesQuery>,
    messages: Messages,
) -> Result<Html<String>, AppError> {
    let sort = query.sort;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Fetch all communities
    let own_communities_raw = match auth_session.user.clone() {
        Some(user) => get_own_communities(&mut tx, user.id).await?,
        None => vec![],
    };

    // The official section must show every official community regardless of
    // which page it would land on, so it is picked from the full list. That
    // query carries no per-community enrichment; the expensive part is bounded
    // below by what actually gets rendered.
    let official_raw: Vec<_> = get_public_communities(&mut tx)
        .await?
        .into_iter()
        .filter(|c| c.owner_login_name == state.config.official_account_login_name)
        .collect();

    let public_communities_raw =
        get_public_communities_paginated(&mut tx, sort, COMMUNITIES_PER_BATCH, 0).await?;
    let public_has_more = public_communities_raw.len() as i64 == COMMUNITIES_PER_BATCH;

    let participating_communities_raw = match auth_session.user.clone() {
        Some(user) => get_participating_communities(&mut tx, user.id).await?,
        None => vec![],
    };

    // Collect all community IDs for batch queries
    let mut all_community_ids: Vec<Uuid> = Vec::new();
    all_community_ids.extend(own_communities_raw.iter().map(|c| c.id));
    all_community_ids.extend(participating_communities_raw.iter().map(|c| c.id));
    all_community_ids.sort();
    all_community_ids.dedup();

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    // Fetch recent posts (3 per community) for all communities
    let recent_posts = find_recent_posts_by_communities(
        &mut tx,
        &all_community_ids,
        3,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    // Fetch members count (unique contributors) and posts count for all communities
    let members_stats = get_communities_members_count(&mut tx, &all_community_ids).await?;

    let community_stats = if !all_community_ids.is_empty() {
        sqlx::query!(
            r#"
            SELECT
                p.community_id,
                COUNT(p.id) as posts_count
            FROM posts p
            WHERE p.community_id = ANY($1)
                AND p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
            GROUP BY p.community_id
            "#,
            &all_community_ids
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };

    // Fetch owner login names for own and participating communities
    let owner_ids: Vec<Uuid> = own_communities_raw
        .iter()
        .chain(participating_communities_raw.iter())
        .map(|c| c.owner_id)
        .collect();

    let owner_logins = if !owner_ids.is_empty() {
        sqlx::query!(
            r#"
            SELECT id, login_name
            FROM users
            WHERE id = ANY($1)
            "#,
            &owner_ids
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };

    // Group posts by community_id
    use std::collections::HashMap as StdHashMap;
    let mut posts_by_community: StdHashMap<Uuid, Vec<serde_json::Value>> = StdHashMap::new();
    // Newest first per community, so the first one seen is the community's last
    // activity. Same trick as enrich_public_communities.
    let mut last_post_by_community: StdHashMap<Uuid, chrono::DateTime<chrono::Utc>> =
        StdHashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            if let Some(published_at) = post.published_at {
                last_post_by_community
                    .entry(community_id)
                    .or_insert(published_at);
            }
            let posts = posts_by_community.entry(community_id).or_default();
            posts.push(serde_json::json!({
                "id": post.id.to_string(),
                "image_filename": post.image_filename,
                "image_width": post.image_width,
                "image_height": post.image_height,
                "author_login_name": post.author_login_name,
            }));
        }
    }

    // Create stats lookup maps
    let mut members_by_community: StdHashMap<Uuid, Option<i64>> = StdHashMap::new();
    for stat in members_stats {
        members_by_community.insert(stat.community_id, stat.members_count);
    }

    let mut posts_count_by_community: StdHashMap<Uuid, Option<i64>> = StdHashMap::new();
    for stat in community_stats {
        if let Some(community_id) = stat.community_id {
            posts_count_by_community.insert(community_id, stat.posts_count);
        }
    }

    // Create owner login lookup map
    let mut owner_login_by_id: StdHashMap<Uuid, String> = StdHashMap::new();
    for owner in owner_logins {
        owner_login_by_id.insert(owner.id, owner.login_name);
    }

    // One "yours" band rather than two: get_own_communities matches on ownership
    // and get_participating_communities on membership, and an owner is normally
    // also a member, so the two lists overlapped and rendered the same community
    // twice before you reached anything new. Deduped by id, owned first, then
    // ordered by last activity so the band leads with whatever is alive.
    let mut seen_your_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut your_communities_sorted: Vec<(
        Option<chrono::DateTime<chrono::Utc>>,
        serde_json::Value,
    )> = own_communities_raw
        .into_iter()
        .chain(participating_communities_raw)
        .filter(|community| seen_your_ids.insert(community.id))
        .map(|community| {
            let recent_posts = posts_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or_default();
            let members_count = members_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or(None);
            let posts_count = posts_count_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or(None);
            let owner_login_name = owner_login_by_id
                .get(&community.owner_id)
                .cloned()
                .unwrap_or_default();
            let last_post_at = last_post_by_community.get(&community.id).copied();

            let card = serde_json::json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "description": community.description,
                "visibility": community.visibility,
                "owner_login_name": owner_login_name,
                "posts_count": posts_count,
                "members_count": members_count,
                "recent_posts": recent_posts,
                "last_post_at": last_post_at,
            });
            (last_post_at, card)
        })
        .collect();
    // Descending, so a community nobody has drawn in sorts last rather than
    // first — which is where a plain sort on Option would put None.
    your_communities_sorted.sort_by_key(|(last_post_at, _)| std::cmp::Reverse(*last_post_at));
    let your_communities: Vec<serde_json::Value> = your_communities_sorted
        .into_iter()
        .map(|(_, card)| card)
        .collect();

    let public_communities = enrich_public_communities(
        &mut tx,
        &public_communities_raw,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let official_communities = enrich_public_communities(
        &mut tx,
        &official_raw,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("communities.jinja")?;
    let rendered = template.clone().render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        sort => sort.as_param(),
        // Same key names the fragment uses, so the first batch and every
        // scrolled batch render through one template.
        has_more => public_has_more,
        next_url => communities_fragment_url(sort, None, COMMUNITIES_PER_BATCH),
        official_communities,
        // Key name must match community_cards_fragment.jinja's loop variable;
        // the page includes that template with this context.
        communities => public_communities,
        your_communities,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang
    })?;

    Ok(Html(rendered))
}

#[derive(Deserialize)]
pub struct CreateCommunityForm {
    name: String,
    slug: String,
    description: String,
    visibility: String,
}

pub async fn do_create_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    messages: Messages,
    Form(form): Form<CreateCommunityForm>,
) -> Result<impl IntoResponse, AppError> {
    if form.name.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Parse visibility from form
    let visibility = match form.visibility.as_str() {
        "public" => CommunityVisibility::Public,
        "unlisted" => CommunityVisibility::Unlisted,
        "private" => CommunityVisibility::Private,
        _ => CommunityVisibility::Public, // Default to public
    };

    // Check if slug conflicts with any user login_name
    if slug_conflicts_with_user(&mut tx, &form.slug).await? {
        let user_preferred_language = auth_session
            .user
            .clone()
            .map(|u| u.preferred_language)
            .unwrap_or_else(|| None);
        let bundle = get_bundle(&accept_language, user_preferred_language);
        let error_message = safe_get_message(&bundle, "community-slug-conflict-error");
        messages.error(error_message);
        return Ok(Redirect::to("/communities/new").into_response());
    }

    let community = create_community(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        CommunityDraft {
            name: form.name,
            slug: form.slug,
            description: form.description,
            visibility,
        },
    )
    .await?;

    // Create actor for the community (only for non-member_only communities)
    if visibility != CommunityVisibility::Private {
        match create_actor_for_community(&mut tx, &community, &state.config).await {
            Ok(_) => {
                let _ = tx.commit().await;
                Ok(Redirect::to(&format!("/@{}", community.slug)).into_response())
            }
            Err(e) => {
                let _ = tx.rollback().await;
                // Check if it's a unique constraint violation (handle conflict)
                if let Some(sqlx::Error::Database(db_err)) = e.downcast_ref::<sqlx::Error>() {
                    if db_err.constraint().is_some() {
                        let user_preferred_language = auth_session
                            .user
                            .clone()
                            .map(|u| u.preferred_language)
                            .unwrap_or_else(|| None);
                        let bundle = get_bundle(&accept_language, user_preferred_language);
                        let error_message =
                            safe_get_message(&bundle, "community-slug-conflict-error");
                        messages.error(error_message);
                        return Ok(Redirect::to("/communities/new").into_response());
                    }
                }
                // For other errors, re-throw
                Err(e.into())
            }
        }
    } else {
        // Member-only community, no actor needed
        let _ = tx.commit().await;
        Ok(Redirect::to(&format!("/communities/@{}", community.slug)).into_response())
    }
}

pub async fn create_community_form(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    messages: Messages,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("create_community.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        messages => messages.into_iter().collect::<Vec<_>>(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered))
}

pub async fn hx_edit_community(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::InvalidFormData("Invalid slug format".to_string()))?
            .to_string();
        find_community_by_slug(&mut tx, slug).await?
    } else {
        // Handle UUID format - redirect to @slug
        let community_uuid = Uuid::parse_str(&id)?;
        let community = find_community_by_id(&mut tx, community_uuid).await?;
        if let Some(community) = &community {
            // Redirect UUID to @slug format
            return Ok(
                Redirect::to(&format!("/communities/@{}/edit", community.slug)).into_response(),
            );
        } else {
            None
        }
    };

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    if community
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Community".to_string()))?
        .owner_id
        != auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("community_edit.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        community,
        community_id => id,
        domain => state.config.domain.clone(),
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn hx_do_edit_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<CreateCommunityForm>,
) -> Result<impl IntoResponse, AppError> {
    if form.name.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let (community_uuid, original_slug) = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::InvalidFormData("Invalid slug format".to_string()))?
            .to_string();
        let community = find_community_by_slug(&mut tx, slug.clone()).await?;
        if let Some(community) = community {
            (community.id, community.slug)
        } else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        }
    } else {
        // Handle UUID format - redirect to @slug
        let uuid = Uuid::parse_str(&id)?;
        let community = find_community_by_id(&mut tx, uuid).await?;
        if let Some(_community) = &community {
            // Redirect UUID to @slug format for PUT request
            return Ok(StatusCode::PERMANENT_REDIRECT.into_response());
        } else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        }
    };

    // Update the community (with ActivityPub Update activity)
    // Parse visibility from form
    let visibility = match form.visibility.as_str() {
        "public" => CommunityVisibility::Public,
        "unlisted" => CommunityVisibility::Unlisted,
        "private" => CommunityVisibility::Private,
        _ => CommunityVisibility::Public, // Default to public
    };

    let community_draft = CommunityDraft {
        name: form.name.clone(),
        slug: form.slug.clone(),
        description: form.description.clone(),
        visibility,
    };

    match update_community_with_activity(
        &mut tx,
        community_uuid,
        community_draft,
        &state.config,
        Some(&state),
    )
    .await
    {
        Ok(updated_community) => {
            // Success - commit transaction
            let _ = tx.commit().await;

            // Check if slug changed - if so, redirect entire page to new URL
            if form.slug != original_slug {
                // Use HTMX redirect to navigate to new slug URL
                Ok(([(
                    "HX-Redirect",
                    format!("/communities/@{}", form.slug).as_str(),
                )],)
                    .into_response())
            } else {
                // Slug didn't change - return updated content block
                let template = state.env.get_template("community.jinja")?;
                let user_preferred_language = auth_session
                    .user
                    .clone()
                    .map(|u| u.preferred_language)
                    .unwrap_or_else(|| None);
                let bundle = get_bundle(&accept_language, user_preferred_language);
                let ftl_lang = bundle
                    .locales
                    .first()
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "en".to_string())
                    .to_string();
                let mut header_tx = state.db_pool.begin().await?;
                let header = community_header_context(&mut header_tx, &updated_community).await?;
                header_tx.commit().await?;
                let rendered = template
                    .render_captured_to(
                        context! {
                            current_user => auth_session.user,
                            header => header,
                            community => updated_community,
                            community_id => updated_community.id.to_string(),
                            domain => state.config.domain.clone(),
                            ftl_lang
                        },
                        std::io::sink(),
                    )?
                    .with_state_mut(|state| state.render_block("community_edit_block"))?;

                Ok(Html(rendered).into_response())
            }
        }
        Err(e) => {
            // Error - rollback transaction and return edit form with error
            let _ = tx.rollback().await;

            // Check if it's a constraint violation (slug conflict)
            let error_message =
                if let Some(sqlx::Error::Database(db_err)) = e.downcast_ref::<sqlx::Error>() {
                    if db_err.constraint().is_some() {
                        let user_preferred_language = auth_session
                            .user
                            .clone()
                            .map(|u| u.preferred_language)
                            .unwrap_or_else(|| None);
                        let bundle = get_bundle(&accept_language, user_preferred_language);
                        Some(safe_get_message(&bundle, "community-slug-conflict-error"))
                    } else {
                        None
                    }
                } else {
                    None
                };

            // Get current community data to show in the form
            let mut tx = db.begin().await?;
            let current_community = find_community_by_id(&mut tx, community_uuid).await?;

            let template = state.env.get_template("community_edit.jinja")?;
            let user_preferred_language = auth_session
                .user
                .clone()
                .map(|u| u.preferred_language)
                .unwrap_or_else(|| None);
            let bundle = get_bundle(&accept_language, user_preferred_language);
            let ftl_lang = bundle
                .locales
                .first()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "en".to_string())
                .to_string();
            let rendered = template.render(context! {
                current_user => auth_session.user,
                community => current_community,
                community_id => id,
                domain => state.config.domain.clone(),
                error_message => error_message,
                ftl_lang
            })?;

            Ok(Html(rendered).into_response())
        }
    }
}

pub async fn community_comments(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
    uri: Uri,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community = if id.starts_with('@') {
        // Handle @slug format
        let slug = id
            .strip_prefix('@')
            .ok_or_else(|| AppError::InvalidFormData("Invalid slug format".to_string()))?
            .to_string();
        find_community_by_slug(&mut tx, slug).await?
    } else {
        // Handle UUID format - redirect to @slug
        let uuid = match parse_id_with_legacy_support(&id, "/communities", &state)? {
            ParsedId::Uuid(uuid) => uuid,
            ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
            ParsedId::InvalidId(error_response) => return Ok(error_response),
        };
        let community = find_community_by_id(&mut tx, uuid).await?;
        if let Some(community) = &community {
            // Redirect UUID to @slug format
            return Ok(
                Redirect::to(&format!("/communities/@{}/comments", community.slug)).into_response(),
            );
        } else {
            None
        }
    };

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;
    let community_uuid = community.id;

    // Access control: verify access based on community visibility
    match community.visibility {
        CommunityVisibility::Private => {
            // Private communities require authentication AND membership
            match &auth_session.user {
                Some(user) => {
                    // User is authenticated, check membership
                    let is_member = is_user_member(&mut tx, user.id, community_uuid).await?;
                    if !is_member {
                        // Authenticated but not a member - show 403 forbidden
                        return Ok(render_403(&auth_session, &state, ftl_lang)
                            .await?
                            .into_response());
                    }
                }
                None => {
                    // Not authenticated - redirect to login with next URL
                    let next_url = uri.path();
                    return Ok(Redirect::to(&format!("/login?next={}", next_url)).into_response());
                }
            }
        }
        CommunityVisibility::Public | CommunityVisibility::Unlisted => {
            // Public and unlisted communities are accessible to everyone
            // No authentication required
        }
    }

    // The whole of what the drawings page's list is the start of, loading
    // as it is scrolled.
    let comments = comments_batch(
        &mut tx,
        CommentScope::Community(community_uuid),
        auth_session.user.as_ref(),
        None,
    )
    .await?;
    let header = community_header_context(&mut tx, &community).await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("community_comments.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        community => community,
        header => header,
        community_id => community_uuid.to_string(),
        comments => comments_context(comments, &community_comments_path(&community.slug)),
        domain => state.config.domain.clone(),
        unread_notification_count => common_ctx.unread_notification_count,
        draft_post_count => common_ctx.draft_post_count,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

// ========== Member Management Endpoints ==========

/// List community members
pub async fn get_members(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Only members can view member list
    let user = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let is_member = is_user_member(&mut tx, user.id, community.id).await?;
    if !is_member {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Fetch members with user details in a single query (no N+1)
    let members = get_community_members_with_details(&mut tx, community.id).await?;

    let members_with_details: Vec<serde_json::Value> = members
        .into_iter()
        .map(|member| {
            serde_json::json!({
                "id": member.id,
                "user_id": member.user_id,
                "login_name": member.login_name,
                "display_name": member.display_name,
                "role": member.role,
                "joined_at": member.joined_at,
            })
        })
        .collect();

    tx.commit().await?;

    Ok(axum::Json(members_with_details).into_response())
}

/// Invite a user to a community
#[derive(Deserialize)]
pub struct InviteUserForm {
    login_name: String,
}

pub async fn invite_user(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    messages: Messages,
    Form(form): Form<InviteUserForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .as_ref()
        .and_then(|u| u.preferred_language.clone());
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let inviter = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if user is owner or moderator
    let role = get_user_role_in_community(&mut tx, inviter.id, community.id).await?;
    match role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Find the invitee by login_name
    let invitee = find_user_by_login_name(&mut tx, &form.login_name).await?;
    if invitee.is_none() {
        messages.error(safe_get_message(&bundle, "community-invite-user-not-found"));
        return Ok(
            Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response(),
        );
    }
    let invitee = invitee.ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // Check if user is already a member
    let already_member = is_user_member(&mut tx, invitee.id, community.id).await?;
    if already_member {
        messages.error(safe_get_message(&bundle, "community-invite-already-member"));
        return Ok(
            Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response(),
        );
    }

    // Create invitation
    match create_invitation(&mut tx, community.id, inviter.id, invitee.id).await {
        Ok(_invitation) => {
            // Get invitee's language preference before committing transaction
            let invitee_language = get_user_language_preference(&mut tx, invitee.id)
                .await
                .ok()
                .flatten();

            tx.commit().await?;

            // Send push notification to invitee with localized message
            let (title, body) = format_community_invitation_message(
                "invite",
                invitee_language,
                &inviter.display_name,
                &community.slug,
            );

            let mut data = serde_json::Map::new();
            data.insert(
                "community_id".to_string(),
                serde_json::json!(community.id.to_string()),
            );
            data.insert(
                "community_slug".to_string(),
                serde_json::json!(community.slug),
            );
            data.insert(
                "notification_type".to_string(),
                serde_json::json!("community_invite"),
            );

            tracing::info!(
                "Sending community invitation push notification to user {}: title={}, body={}",
                invitee.id,
                title,
                body
            );

            // The number on the bell, for the icon's badge
            let mut badge_tx = db.begin().await?;
            let unread_count =
                crate::models::notification::get_badge_count(&mut badge_tx, invitee.id)
                    .await
                    .ok();
            let _ = badge_tx.commit().await;

            // Send push notification (don't fail if this errors)
            match state
                .push_service
                .send_notification_to_user(
                    invitee.id,
                    &title,
                    &body,
                    unread_count.map(|c| c as u32), // badge count
                    "/notifications",
                    data,
                )
                .await
            {
                Ok(_) => {
                    tracing::info!(
                        "Successfully sent community invitation push notification to user {}",
                        invitee.id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to send community invitation push notification to user {}: {:?}",
                        invitee.id,
                        e
                    );
                }
            }

            messages.success(safe_get_message(&bundle, "community-invite-success"));

            Ok(Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response())
        }
        Err(e) => {
            // Check if this is a duplicate key constraint error
            if let Some(sqlx::Error::Database(ref err)) = e.downcast_ref::<sqlx::Error>() {
                if err.is_unique_violation() {
                    messages.error(safe_get_message(
                        &bundle,
                        "community-invite-already-invited",
                    ));
                    return Ok(
                        Redirect::to(&format!("/communities/@{}/members", community.slug))
                            .into_response(),
                    );
                }
            }
            // For other errors, propagate them
            Err(e.into())
        }
    }
}

/// Remove a member from a community
pub async fn remove_member(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((slug, user_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let current_user = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if current user is owner or moderator
    let current_role = get_user_role_in_community(&mut tx, current_user.id, community.id).await?;
    match current_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Cannot remove the owner
    let target_role = get_user_role_in_community(&mut tx, user_id, community.id).await?;
    if target_role == Some(CommunityMemberRole::Owner) {
        return Ok((StatusCode::BAD_REQUEST, "Cannot remove community owner").into_response());
    }

    // Remove the member
    remove_community_member(&mut tx, community.id, user_id).await?;

    tx.commit().await?;

    // Return empty HTML for HTMX to remove the row
    Ok(Html(String::new()).into_response())
}

// ========== Invitation Endpoints ==========

/// Accept an invitation
pub async fn do_accept_invitation(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(invitation_id): Path<Uuid>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the invitation
    let invitation = get_invitation_by_id(&mut tx, invitation_id).await?;
    if invitation.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let invitation = invitation.ok_or_else(|| AppError::NotFound("Invitation".to_string()))?;

    // Verify the invitation is for the current user
    if invitation.invitee_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Get community info for validation and push notification
    let community = find_community_by_id(&mut tx, invitation.community_id).await?;
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Store inviter_id before consuming invitation
    let inviter_id = invitation.inviter_id;

    // Accept the invitation
    accept_invitation(&mut tx, invitation_id).await?;

    // Add user as a member
    add_community_member(
        &mut tx,
        invitation.community_id,
        user.id,
        CommunityMemberRole::Member,
        Some(inviter_id),
    )
    .await?;

    // Get inviter's language preference before committing transaction
    let inviter_language = get_user_language_preference(&mut tx, inviter_id)
        .await
        .ok()
        .flatten();

    tx.commit().await?;
    state.push_service.refresh_badge(user.id);

    // Send push notification to inviter with localized message
    let (title, body) = format_community_invitation_message(
        "accepted",
        inviter_language,
        &user.display_name,
        &community.slug,
    );

    let mut data = serde_json::Map::new();
    data.insert(
        "community_id".to_string(),
        serde_json::json!(community.id.to_string()),
    );
    data.insert(
        "community_slug".to_string(),
        serde_json::json!(community.slug),
    );
    data.insert(
        "notification_type".to_string(),
        serde_json::json!("invitation_accepted"),
    );

    tracing::info!(
        "Sending invitation accepted push notification to user {}: title={}, body={}",
        inviter_id,
        title,
        body
    );

    // The number on the bell, for the icon's badge
    let mut badge_tx = db.begin().await?;
    let unread_count = crate::models::notification::get_badge_count(&mut badge_tx, inviter_id)
        .await
        .ok();
    let _ = badge_tx.commit().await;

    // Send push notification (don't fail if this errors)
    match state
        .push_service
        .send_notification_to_user(
            inviter_id,
            &title,
            &body,
            unread_count.map(|c| c as u32), // badge count
            &format!("/communities/@{}/members", community.slug),
            data,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(
                "Successfully sent invitation accepted push notification to user {}",
                inviter_id
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to send invitation accepted push notification to user {}: {:?}",
                inviter_id,
                e
            );
        }
    }

    messages.success(safe_get_message(&bundle, "invitation-accepted"));

    Ok(Redirect::to("/notifications").into_response())
}

/// Reject an invitation
pub async fn do_reject_invitation(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(invitation_id): Path<Uuid>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the invitation
    let invitation = get_invitation_by_id(&mut tx, invitation_id).await?;
    if invitation.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let invitation = invitation.ok_or_else(|| AppError::NotFound("Invitation".to_string()))?;

    // Verify the invitation is for the current user
    if invitation.invitee_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Get community info for push notification
    let community = find_community_by_id(&mut tx, invitation.community_id).await?;
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Store inviter_id before consuming invitation
    let inviter_id = invitation.inviter_id;

    // Reject the invitation
    reject_invitation(&mut tx, invitation_id).await?;

    // Get inviter's language preference before committing transaction
    let inviter_language = get_user_language_preference(&mut tx, inviter_id)
        .await
        .ok()
        .flatten();

    tx.commit().await?;
    state.push_service.refresh_badge(user.id);

    // Send push notification to inviter with localized message
    let (title, body) = format_community_invitation_message(
        "declined",
        inviter_language,
        &user.display_name,
        &community.slug,
    );

    let mut data = serde_json::Map::new();
    data.insert(
        "community_id".to_string(),
        serde_json::json!(community.id.to_string()),
    );
    data.insert(
        "community_slug".to_string(),
        serde_json::json!(community.slug),
    );
    data.insert(
        "notification_type".to_string(),
        serde_json::json!("invitation_rejected"),
    );

    tracing::info!(
        "Sending invitation rejected push notification to user {}: title={}, body={}",
        inviter_id,
        title,
        body
    );

    // The number on the bell, for the icon's badge
    let mut badge_tx = db.begin().await?;
    let unread_count = crate::models::notification::get_badge_count(&mut badge_tx, inviter_id)
        .await
        .ok();
    let _ = badge_tx.commit().await;

    // Send push notification (don't fail if this errors)
    match state
        .push_service
        .send_notification_to_user(
            inviter_id,
            &title,
            &body,
            unread_count.map(|c| c as u32), // badge count
            &format!("/communities/@{}/members", community.slug),
            data,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(
                "Successfully sent invitation rejected push notification to user {}",
                inviter_id
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to send invitation rejected push notification to user {}: {:?}",
                inviter_id,
                e
            );
        }
    }

    messages.success(safe_get_message(&bundle, "invitation-rejected"));

    Ok(Redirect::to("/notifications").into_response())
}

/// Retract/cancel a pending invitation
pub async fn retract_invitation(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((slug, invitation_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if user is owner or moderator
    let user_role = get_user_role_in_community(&mut tx, user.id, community.id).await?;
    match user_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Delete the invitation
    let invitee_id = sqlx::query_scalar!(
        "DELETE FROM community_invitations WHERE id = $1 AND community_id = $2 AND status = 'pending' RETURNING invitee_id",
        invitation_id,
        community.id
    )
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;
    if let Some(invitee_id) = invitee_id {
        state.push_service.refresh_badge(invitee_id);
    }

    // Return empty HTML for HTMX to remove the row
    Ok(Html(String::new()).into_response())
}

/// Render members management page
pub async fn members_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // For private/unlisted communities, only members can view member list
    // For public communities, anyone can view
    let user_role = if community.visibility != crate::models::community::CommunityVisibility::Public
    {
        // Private or unlisted community - require membership
        let user = match &auth_session.user {
            Some(user) => user,
            None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
        };

        let is_member = is_user_member(&mut tx, user.id, community.id).await?;
        if !is_member {
            return Ok(StatusCode::FORBIDDEN.into_response());
        }

        // Get user's role to determine permissions
        get_user_role_in_community(&mut tx, user.id, community.id).await?
    } else {
        // Public community - anyone can view, but only logged-in members have roles
        match &auth_session.user {
            Some(user) => get_user_role_in_community(&mut tx, user.id, community.id).await?,
            None => None,
        }
    };

    // Fetch members with user details in a single query (no N+1)
    let members = get_community_members_with_details(&mut tx, community.id).await?;

    let members_with_details: Vec<serde_json::Value> = members
        .into_iter()
        .map(|member| {
            serde_json::json!({
                "id": member.id,
                "user_id": member.user_id,
                "login_name": member.login_name,
                "display_name": member.display_name,
                "role": member.role,
                "joined_at": member.joined_at,
            })
        })
        .collect();

    // Fetch pending invitations with invitee details in a single query (no N+1)
    let pending_invitations = match user_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {
            let invitations =
                get_pending_invitations_with_invitee_details_for_community(&mut tx, community.id)
                    .await?;
            invitations
                .into_iter()
                .map(|invitation| {
                    serde_json::json!({
                        "id": invitation.id,
                        "invitee_login_name": invitation.invitee_login_name,
                        "invitee_display_name": invitation.invitee_display_name,
                        "created_at": invitation.created_at,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("community_members.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        community,
        members => members_with_details,
        pending_invitations,
        user_role,
        can_invite => matches!(user_role, Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator)),
        can_remove => matches!(user_role, Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator)),
        messages => messages.into_iter().collect::<Vec<_>>(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

/// Leave a community (HTMX)
pub async fn do_leave_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find community
    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;
    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Leave the community (will check if user is owner/member inside)
    match leave_community(&mut tx, community.id, user.id).await {
        Ok(_) => {
            tx.commit().await?;
            messages.success(safe_get_message(&bundle, "community-left-success"));
            Ok(Redirect::to("/communities").into_response())
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("Owners cannot leave") {
                messages.error(safe_get_message(&bundle, "community-owner-cannot-leave"));
            } else if error_msg.contains("not a member") {
                return Ok(StatusCode::NOT_FOUND.into_response());
            } else {
                messages.error(format!("Error: {}", error_msg));
            }
            Ok(Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response())
        }
    }
}

/// DELETE handler for web interface (HTMX)
pub async fn hx_delete_community(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Verify user is authenticated
    let user = match &auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Attempt to delete the community
    soft_delete_community_with_activity(&mut tx, &slug, user.id, &state.config, Some(&state))
        .await?;

    tx.commit().await?;

    // Redirect to communities list
    Ok(([("HX-Redirect", "/communities")],).into_response())
}

#[cfg(test)]
mod tests {
    use super::communities_fragment_url;
    use crate::models::community::CommunitySort;
    use crate::web::handlers::test_support;
    use minijinja::context;
    use serde_json::json;

    fn sample_community() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "name": "Open Studio",
            "slug": "open",
            "description": "a place",
            "visibility": "public",
            "owner_login_name": "someone",
            "posts_count": 12,
            "members_count": 4,
            "recent_posts": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "image_filename": "abcdef.png",
                "image_width": 300,
                "image_height": 300,
                "author_login_name": "someone",
            }],
            "last_post_at": "2026-01-02T03:04:05Z",
        })
    }

    fn directory_context(
        your_communities: Vec<serde_json::Value>,
        current_user: serde_json::Value,
    ) -> minijinja::Value {
        context! {
            current_user => current_user,
            messages => Vec::<serde_json::Value>::new(),
            your_communities => your_communities,
            official_communities => Vec::<serde_json::Value>::new(),
            communities => vec![sample_community()],
            sort => "active",
            has_more => true,
            next_url => "/api/communities/cards?offset=20&sort=active",
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    #[test]
    fn sentinel_url_round_trips_sort_and_search() {
        // The sentinel has to carry both, or scrolling a sorted or searched
        // directory silently reverts to the default listing.
        let url = communities_fragment_url(CommunitySort::Posts, None, 20);
        assert_eq!(url, "/api/communities/cards?offset=20&sort=posts");

        let url = communities_fragment_url(CommunitySort::Name, Some("art club"), 40);
        assert_eq!(
            url,
            "/api/communities/cards?offset=40&sort=name&q=art%20club"
        );
    }

    #[test]
    fn blank_search_is_not_carried_into_the_sentinel() {
        let url = communities_fragment_url(CommunitySort::Active, Some("   "), 20);
        assert_eq!(url, "/api/communities/cards?offset=20&sort=active");
    }

    #[test]
    fn directory_page_renders_its_first_batch() {
        // Regression: the page passed `public_communities` while the included
        // fragment looped over `communities`, so the first batch silently
        // rendered empty and the sentinel skipped straight to offset=20.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("communities.jinja renders");
        assert!(
            rendered.contains("class=\"community-card\""),
            "first batch did not render inside the page"
        );
        assert!(rendered.contains("Open Studio"));
        assert!(rendered.contains("infinite-scroll-sentinel"));
        // The directory is a grid in the wide container, like the home feed.
        // At .center it fit exactly one community per row.
        assert!(rendered.contains("class=\"center-wide communities-page\""));
        assert!(rendered.contains("class=\"community-grid\""));
    }

    #[test]
    fn card_shows_last_activity_and_hides_the_slug() {
        // The directory defaults to sorting on last activity, so the card has
        // to show it. The slug does not appear: it links where the name already
        // links, and for most communities it is a raw UUID.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("renders");
        assert!(rendered.contains("2026-01-02"), "last activity not shown");
        assert!(
            !rendered.contains("&gt;@open&lt;") && !rendered.contains(">@open<"),
            "slug rendered as its own line again"
        );
        // The owner handle is still credited.
        assert!(rendered.contains("@someone"));
    }

    #[test]
    fn a_community_with_no_posts_still_renders_a_card() {
        // Owned-but-empty communities appear in the "yours" band, where they
        // have no thumbnails and no last-activity date. Both used to be the
        // only things on the card with any height.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let mut empty = sample_community();
        empty["name"] = json!("Nothing Yet");
        empty["recent_posts"] = json!([]);
        empty["posts_count"] = json!(0);
        empty["last_post_at"] = json!(null);
        let rendered = template
            .render(directory_context(
                vec![empty],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        assert!(rendered.contains("Nothing Yet"));
        assert!(rendered.contains("community-card-empty"));
    }

    #[test]
    fn yours_is_a_view_beside_the_directory() {
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(
                vec![sample_community()],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        // One view at a time: the public directory shows first and the
        // others wait behind their tabs.
        assert!(rendered.contains("data-communities-tab=\"yours\""));
        assert!(rendered.contains("data-communities-panel=\"yours\" hidden"));
        assert!(rendered.contains("data-communities-panel=\"public\">"));
        // One list, not two: the participating section was a second copy of
        // every community you both own and are a member of.
        assert!(!rendered.contains("participating-community"));
        // Signed out, there is no Yours and nothing to create.
        let rendered = template
            .render(directory_context(Vec::new(), json!(null)))
            .expect("renders");
        assert!(!rendered.contains("data-communities-tab=\"yours\""));
        assert!(!rendered.contains("/communities/new"));
    }

    #[test]
    fn search_belongs_to_the_list_it_actually_filters() {
        // Its hx-target has always been the public list alone, so it is shown
        // with that list and hidden with it.
        let env = test_support::env();
        let template = env
            .get_template("communities.jinja")
            .expect("template loads");
        let rendered = template
            .render(directory_context(
                vec![sample_community()],
                json!({"login_name": "someone"}),
            ))
            .expect("renders");
        let filters = rendered
            .find("data-communities-for=\"public\"")
            .expect("filters");
        let search = rendered
            .find("id=\"community-search\"")
            .expect("search box");
        let sort = rendered.find("name=\"sort\"").expect("sort");
        let panel = rendered.find("data-communities-panel=").expect("panels");
        assert!(filters < search && search < sort && sort < panel);
    }

    #[test]
    fn renders_community_cards_fragment_standalone() {
        let env = test_support::env();
        let template = env
            .get_template("community_cards_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                communities => vec![sample_community()],
                has_more => true,
                next_url => "/api/communities/cards?offset=20&sort=posts",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders standalone");
        assert!(rendered.contains(r#"href="/@open""#));
        assert!(rendered.contains("hx-trigger=\"revealed\""));
    }

    #[test]
    fn fragment_shows_empty_state_and_no_sentinel() {
        let env = test_support::env();
        let template = env
            .get_template("community_cards_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                communities => Vec::<serde_json::Value>::new(),
                has_more => false,
                next_url => "",
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");
        assert!(!rendered.contains("infinite-scroll-sentinel"));
        assert!(rendered.contains("active-communities-nil"));
    }
}
