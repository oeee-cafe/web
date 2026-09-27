//! A community's page: its drawings, what has been said on them, and the embed.

use crate::app_error::AppError;
use crate::models::comment::CommentScope;
use crate::models::community::{
    find_community_by_id, find_community_by_slug, get_community_stats, is_user_member, Community,
    CommunityVisibility,
};
use crate::models::post::find_published_posts_by_community_id;
use crate::models::user::{find_user_by_id, AuthSession};
use crate::web::handlers::home::{
    comments_batch, comments_context, feed_context, CommentsQuery, LoadMoreQuery,
    HOME_POSTS_PER_BATCH,
};
use crate::web::handlers::{parse_id_with_legacy_support, ParsedId};
use crate::web::state::AppState;
use axum::extract::{Path, Query};
use axum::http::{uri::Uri, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html};
use minijinja::context;

use crate::web::context::CommonContext;
use crate::web::i18n::ExtractFtlLang;

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
            .ok_or_else(|| AppError::BadRequest("Invalid slug format".to_string()))?
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
pub(super) async fn community_header_context(
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
                        return Err(AppError::Forbidden);
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

    let template = "community.jinja";

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
        let rendered = state
            .render_block(
                template,
                "community_edit_block",
                context! {
                    current_user => auth_session.user,
                    community => Some(&community),
                    header => header,
                    community_id => community_uuid.to_string(),
                    domain => state.config.domain.clone(),
                    ftl_lang
                },
            )
            .await?;
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
    let common_ctx = CommonContext::build(tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                community => Some(&community),
                header => header,
                community_id => community_uuid.to_string(),
                domain => state.config.domain.clone(),
                feed => feed_context(posts, &community_posts_path(&community.slug), 0, None),
                comments => comments_context(comments, &community_comments_path(&community.slug)),
            },
        )
        .await?;
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
        .render(
            "comments_fragment.jinja",
            context! {
                comments => comments_context(comments, &community_comments_path(&community.slug)),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
                ftl_lang,
            },
        )
        .await?;
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

    let template = "post_feed_fragment.jinja";
    let rendered = state
        .render(
            template,
            context! {
                feed => feed_context(
                    posts,
                    &community_posts_path(&community.slug),
                    query.offset,
                    query.period.as_deref(),
                ),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
                ftl_lang,
            },
        )
        .await?;

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
            .ok_or_else(|| AppError::BadRequest("Invalid slug format".to_string()))?
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
                        return Err(AppError::Forbidden);
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

    let rendered = state
        .render(
            "community_iframe.jinja",
            context! {
                current_user => auth_session.user,
                community => community,
                posts,
                ftl_lang,
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
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
            .ok_or_else(|| AppError::BadRequest("Invalid slug format".to_string()))?
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
                        return Err(AppError::Forbidden);
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
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let template = "community_comments.jinja";
    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                community => community,
                header => header,
                community_id => community_uuid.to_string(),
                comments => comments_context(comments, &community_comments_path(&community.slug)),
                domain => state.config.domain.clone(),
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}
