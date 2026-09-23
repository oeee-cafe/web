use super::ExtractFtlLang;
use crate::app_error::{error_codes, AppError};
use crate::models::actor::Actor;
use crate::models::comment::{
    build_comment_thread_tree_paginated, create_comment,
    find_latest_comments_from_public_communities, CommentDraft,
};
use crate::models::community::{
    get_communities_members_count, get_public_communities, is_user_member, Community,
};
use crate::models::hashtag::{get_hashtags_for_post, set_post_hashtags};
use crate::models::notification::{
    create_notification, get_notification_by_id, get_unread_count, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::{
    build_thread_tree, delete_post_with_activity, edit_post, find_following_posts_by_user_id,
    find_member_community_posts, find_popular_posts, find_post_by_id, find_post_detail_for_json,
    find_public_posts,
    find_recent_posts_by_communities, SerializableThreadedPost,
};
use crate::models::reaction::{
    create_reaction, delete_reaction, find_reactions_by_post_id_and_emoji, find_user_reaction,
    get_reaction_counts, ReactionDraft,
};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::responses::{
    AuthorInfo, ChildPostAuthor, ChildPostImage, ChildPostResponse, CommentListResponse,
    CommentWithPost, CommentsListResponse, CommunityListResponse, CommunityPostThumbnail,
    CommunityWithPosts, ErrorResponse, ImageInfo, PaginationMeta, PostCommunityInfo, PostDetail,
    PostDetailResponse, PostListResponse, PostThumbnail, ReactionCount, ReactionsDetailResponse,
    Reactor, ThreadedCommentResponse,
};
use crate::web::state::AppState;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{extract::State, response::Html, response::Json};
use axum_messages::Messages;
use serde::{Deserialize, Serialize};
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
    /// `/popular`: the past six months', the most reacted to first.
    Popular,
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
            Feed::Popular => "popular",
            Feed::Following => "following",
            Feed::Communities => "communities",
        }
    }

    /// Where the next batch comes from.
    fn batch_path(self) -> &'static str {
        match self {
            Feed::Recent => "/api/home/posts",
            Feed::Popular => "/api/popular/posts",
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
            Feed::Popular => find_popular_posts(tx, limit, offset, viewer_id, show_sensitive).await?,
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

pub async fn popular(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    feed_page(Feed::Popular, auth_session, state, ftl_lang, messages).await
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

#[derive(Deserialize)]
pub struct CommentsQuery {
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_comments_limit")]
    pub limit: i64,
}

fn default_comments_limit() -> i64 {
    100
}

#[derive(Deserialize)]
pub struct CreateCommentRequest {
    pub content: String,
    pub parent_comment_id: Option<String>,
}

pub async fn load_more_public_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Recent, auth_session, state, query).await
}

/// GET /api/popular/posts
pub async fn load_more_popular_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    feed_batch(Feed::Popular, auth_session, state, query).await
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

pub async fn load_more_public_posts_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<Json<PostListResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    let posts = find_public_posts(
        &mut tx,
        query.limit,
        query.offset,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    tx.commit().await?;

    let thumbnails: Vec<PostThumbnail> = posts
        .into_iter()
        .map(|post| {
            let image_prefix = &post.image_filename[..2];
            PostThumbnail {
                id: post.id,
                image_url: format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, post.image_filename
                ),
                image_width: post.image_width,
                image_height: post.image_height,
                is_sensitive: post.is_sensitive,
            }
        })
        .collect();

    let has_more = thumbnails.len() as i64 == query.limit;

    Ok(Json(PostListResponse {
        posts: thumbnails,
        pagination: PaginationMeta {
            offset: query.offset + query.limit,
            limit: query.limit,
            total: None,
            has_more,
        },
    }))
}

pub async fn get_active_communities_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<CommunityListResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let (viewer_user_id, viewer_show_sensitive) = if let Some(ref user) = auth_session.user {
        (Some(user.id), user.show_sensitive_content)
    } else {
        (None, false)
    };

    let active_public_communities_raw = get_public_communities(&mut tx).await?;

    // Filter to communities with at least 10 posts, then limit to 5 communities
    let active_public_communities_raw: Vec<_> = active_public_communities_raw
        .into_iter()
        .filter(|c| c.posts_count.unwrap_or(0) >= 10)
        .take(5)
        .collect();

    // Fetch recent posts and stats for active communities
    let community_ids: Vec<uuid::Uuid> =
        active_public_communities_raw.iter().map(|c| c.id).collect();

    let recent_posts = find_recent_posts_by_communities(
        &mut tx,
        &community_ids,
        10,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    let community_stats = get_communities_members_count(&mut tx, &community_ids).await?;

    tx.commit().await?;

    // Group posts by community_id
    use std::collections::HashMap;
    let mut posts_by_community: HashMap<uuid::Uuid, Vec<CommunityPostThumbnail>> = HashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            let posts = posts_by_community.entry(community_id).or_default();
            let image_prefix = &post.image_filename[..2];
            posts.push(CommunityPostThumbnail {
                id: post.id,
                image_url: format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, post.image_filename
                ),
                image_width: post.image_width,
                image_height: post.image_height,
                is_sensitive: post.is_sensitive,
            });
        }
    }

    // Create stats lookup map
    let mut stats_by_community: HashMap<uuid::Uuid, Option<i64>> = HashMap::new();
    for stat in community_stats {
        stats_by_community.insert(stat.community_id, stat.members_count);
    }

    // Build active communities with all metadata
    let communities: Vec<CommunityWithPosts> = active_public_communities_raw
        .into_iter()
        .map(|community| {
            let recent_posts = posts_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or_default();
            let members_count = stats_by_community.get(&community.id).cloned().flatten();

            CommunityWithPosts {
                id: community.id,
                name: community.name,
                slug: community.slug,
                description: community.description,
                visibility: community.visibility,
                owner_login_name: community.owner_login_name,
                posts_count: community.posts_count,
                members_count,
                recent_posts,
            }
        })
        .collect();

    Ok(Json(CommunityListResponse { communities }))
}

pub async fn get_latest_comments_json(
    _auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<CommentListResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let recent_comments = find_latest_comments_from_public_communities(&mut tx, 10).await?;

    tx.commit().await?;

    let comments: Vec<CommentWithPost> = recent_comments
        .into_iter()
        .map(|comment| {
            let post_image_url = comment.post_image_filename.as_ref().map(|filename| {
                let image_prefix = &filename[..2];
                format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, filename
                )
            });

            CommentWithPost {
                id: comment.id,
                post_id: comment.post_id,
                actor_id: comment.actor_id,
                content: comment.content,
                content_html: comment.content_html,
                actor_name: comment.actor_name,
                actor_handle: comment.actor_handle,
                actor_login_name: comment.actor_login_name,
                is_local: comment.is_local,
                created_at: comment.created_at,
                post_title: comment.post_title,
                post_author_login_name: comment.post_author_login_name,
                post_image_url,
                post_image_width: comment.post_image_width,
                post_image_height: comment.post_image_height,
            }
        })
        .collect();

    Ok(Json(CommentListResponse { comments }))
}

/// Helper function to convert SerializableThreadedPost to ChildPostResponse
fn threaded_post_to_response(
    post: SerializableThreadedPost,
    r2_endpoint: &str,
) -> ChildPostResponse {
    let image_prefix = &post.image_filename[..2];
    ChildPostResponse {
        id: post.id,
        title: post.title,
        content: post.content,
        author: ChildPostAuthor {
            id: post.author_id,
            login_name: post.user_login_name,
            display_name: post.user_display_name,
            actor_handle: post.user_actor_handle,
        },
        image: ChildPostImage {
            url: format!(
                "{}/image/{}/{}",
                r2_endpoint, image_prefix, post.image_filename
            ),
            width: post.image_width,
            height: post.image_height,
        },
        published_at: post.published_at,
        comments_count: post.comments_count,
        children: post
            .children
            .into_iter()
            .map(|child| threaded_post_to_response(child, r2_endpoint))
            .collect(),
    }
}

pub async fn get_post_details_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
) -> Result<Json<PostDetailResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get post details with proper types
    let post_data = find_post_detail_for_json(&mut tx, post_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Post not found"))?;

    // Get parent post if it exists
    let parent_post = if let Some(parent_id) = post_data.parent_post_id {
        let parent_data = find_post_detail_for_json(&mut tx, parent_id).await?;
        parent_data.map(|p| {
            let image_prefix = &p.image_filename[..2];
            ChildPostResponse {
                id: p.id,
                title: p.title,
                content: p.content,
                author: ChildPostAuthor {
                    id: p.author_id,
                    login_name: p.login_name.clone(),
                    display_name: p.display_name,
                    actor_handle: format!("{}@{}", p.login_name, state.config.domain),
                },
                image: ChildPostImage {
                    url: format!(
                        "{}/image/{}/{}",
                        state.config.r2_public_endpoint_url, image_prefix, p.image_filename
                    ),
                    width: p.image_width,
                    height: p.image_height,
                },
                published_at: p.published_at_utc.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                }),
                comments_count: 0,    // Not needed for parent display
                children: Vec::new(), // Parent post doesn't show its own children in this context
            }
        })
    } else {
        None
    };

    // Get child posts (replies) using threaded structure
    let child_posts = build_thread_tree(&mut tx, post_id).await?;

    // Get reaction counts
    let user_actor_id = if let Some(ref user) = auth_session.user {
        Actor::find_by_user_id(&mut tx, user.id)
            .await
            .ok()
            .flatten()
            .map(|actor| actor.id)
    } else {
        None
    };
    let reactions = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;

    // Get hashtags for this post
    let hashtags_data = get_hashtags_for_post(&mut tx, post_id).await?;
    let hashtags: Vec<String> = hashtags_data.into_iter().map(|h| h.display_name).collect();

    tx.commit().await?;

    let post = PostDetail {
        id: post_data.id,
        title: post_data.title,
        content: post_data.content,
        author: AuthorInfo {
            id: post_data.author_id,
            login_name: post_data.login_name,
            display_name: post_data.display_name,
        },
        viewer_count: post_data.viewer_count,
        image: ImageInfo {
            filename: post_data.image_filename,
            width: post_data.image_width,
            height: post_data.image_height,
            tool: post_data.image_tool,
            paint_duration: post_data.paint_duration,
        },
        is_sensitive: post_data.is_sensitive,
        allow_relay: post_data.allow_relay,
        allow_replay: post_data.allow_replay,
        published_at_utc: post_data.published_at_utc,
        community: match (
            post_data.community_id,
            post_data.community_name,
            post_data.community_slug,
        ) {
            (Some(id), Some(name), Some(slug)) => Some(PostCommunityInfo {
                id,
                name,
                slug,
                background_color: post_data.community_background_color,
                foreground_color: post_data.community_foreground_color,
            }),
            _ => None,
        },
        hashtags,
    };

    let child_posts_response: Vec<ChildPostResponse> = child_posts
        .into_iter()
        .map(|child| threaded_post_to_response(child, &state.config.r2_public_endpoint_url))
        .collect();

    let reactions_response: Vec<ReactionCount> = reactions
        .into_iter()
        .map(|r| ReactionCount {
            emoji: r.emoji,
            count: r.count,
            reacted_by_user: r.reacted_by_user,
        })
        .collect();

    Ok(Json(PostDetailResponse {
        post,
        parent_post,
        child_posts: child_posts_response,
        reactions: reactions_response,
    }))
}

pub async fn get_post_reactions_by_emoji_json(
    _auth_session: AuthSession,
    State(state): State<AppState>,
    Path((post_id, emoji)): Path<(Uuid, String)>,
) -> Result<Json<ReactionsDetailResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get reactions for this post and emoji
    let reactions_data = find_reactions_by_post_id_and_emoji(&mut tx, post_id, &emoji).await?;

    tx.commit().await?;

    let reactions: Vec<Reactor> = reactions_data
        .into_iter()
        .map(|r| Reactor {
            iri: r.iri,
            post_id: r.post_id,
            actor_id: r.actor_id,
            emoji: r.emoji,
            created_at: r.created_at,
            actor_name: r.actor_name,
            actor_handle: r.actor_handle,
        })
        .collect();

    Ok(Json(ReactionsDetailResponse { reactions }))
}

pub async fn get_post_comments_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Query(query): Query<CommentsQuery>,
) -> Result<Json<CommentsListResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Validate and cap the limit
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);

    // Get post's community_id to check visibility
    let post_community_id = sqlx::query_scalar!(
        r#"
        SELECT community_id
        FROM posts
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        post_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Post not found"))?;

    // Get the community to check visibility
    let community = sqlx::query_as!(
        Community,
        r#"
        SELECT id, slug, name, description, owner_id, visibility AS "visibility: _", created_at, updated_at, background_color, foreground_color
        FROM communities
        WHERE id = $1
        "#,
        post_community_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    // Check authorization for private communities
    if let Some(ref community) = community {
        if community.visibility == crate::models::community::CommunityVisibility::Private {
            // For private communities, user must be logged in and be a member
            if let Some(ref user) = auth_session.user {
                let is_member = is_user_member(&mut tx, user.id, community.id).await?;
                if !is_member {
                    return Err(anyhow::anyhow!(
                        "You must be a member to view comments in this private community"
                    )
                    .into());
                }
            } else {
                return Err(anyhow::anyhow!(
                    "Authentication required to view comments in private community"
                )
                .into());
            }
        }
    }

    // Get paginated comments
    let (comments_data, _total_count) =
        build_comment_thread_tree_paginated(&mut tx, post_id, limit, offset).await?;

    tx.commit().await?;

    // Convert to response format
    fn convert_to_response(
        comment: crate::models::comment::SerializableThreadedComment,
    ) -> ThreadedCommentResponse {
        ThreadedCommentResponse {
            id: comment.id,
            post_id: comment.post_id,
            parent_comment_id: comment.parent_comment_id,
            actor_id: comment.actor_id,
            content: comment.content,
            content_html: comment.content_html,
            actor_name: comment.actor_name,
            actor_handle: comment.actor_handle,
            actor_login_name: comment.actor_login_name,
            is_local: comment.is_local,
            created_at: comment.created_at,
            updated_at: comment.updated_at,
            deleted_at: comment.deleted_at,
            children: comment
                .children
                .into_iter()
                .map(convert_to_response)
                .collect(),
        }
    }

    let comments: Vec<ThreadedCommentResponse> =
        comments_data.into_iter().map(convert_to_response).collect();

    // Determine if there are more comments
    let has_more = comments.len() as i64 == limit;

    Ok(Json(CommentsListResponse {
        comments,
        pagination: PaginationMeta {
            offset: offset + limit,
            limit,
            total: None,
            has_more,
        },
    }))
}

pub async fn create_comment_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Json(request): Json<CreateCommentRequest>,
) -> Result<Json<ThreadedCommentResponse>, AppError> {
    // Require authentication
    let user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("Authentication required"))?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Parse parent_comment_id if provided
    let parent_comment_id = request
        .parent_comment_id
        .as_ref()
        .and_then(|id| Uuid::parse_str(id).ok());

    // Get the post to check access and get author
    let post = sqlx::query!(
        r#"
        SELECT author_id, community_id
        FROM posts
        WHERE id = $1
        "#,
        post_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Post not found"))?;

    // Check if post is in a private community and if user has access
    let community = sqlx::query_as!(
        Community,
        r#"
        SELECT id, slug, name, description, owner_id, visibility AS "visibility: _", created_at, updated_at, background_color, foreground_color
        FROM communities
        WHERE id = $1
        "#,
        post.community_id
    )
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(ref comm) = community {
        if comm.visibility == crate::models::community::CommunityVisibility::Private {
            let is_member = is_user_member(&mut tx, user.id, comm.id).await?;
            if !is_member {
                return Err(anyhow::anyhow!(
                    "You must be a member to comment in this private community"
                )
                .into());
            }
        }
    }

    // Create the comment
    let comment = create_comment(
        &mut tx,
        CommentDraft {
            actor_id: actor.id,
            post_id,
            parent_comment_id,
            content: request.content,
            content_html: None,
        },
    )
    .await?;

    // Collect notification info for push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // If this is a reply to another comment, notify the parent comment author
    if let Some(parent_id) = parent_comment_id {
        let parent_comment = sqlx::query!(
            r#"
            SELECT actor_id
            FROM comments
            WHERE id = $1
            "#,
            parent_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(parent) = parent_comment {
            let parent_actor = sqlx::query!(
                r#"
                SELECT user_id
                FROM actors
                WHERE id = $1
                "#,
                parent.actor_id
            )
            .fetch_optional(&mut *tx)
            .await?;

            if let Some(parent_actor_data) = parent_actor {
                if let Some(parent_user_id) = parent_actor_data.user_id {
                    if parent_user_id != user.id {
                        if let Ok(notification) = create_notification(
                            &mut tx,
                            CreateNotificationParams {
                                recipient_id: parent_user_id,
                                actor_id: actor.id,
                                notification_type: NotificationType::CommentReply,
                                post_id: Some(post_id),
                                comment_id: Some(comment.id),
                                reaction_iri: None,
                                guestbook_entry_id: None,
                            },
                        )
                        .await
                        {
                            notification_info.push((notification.id, parent_user_id));
                        }
                    }
                }
            }
        }
    } else {
        // Notify the post author for top-level comments
        if post.author_id != user.id {
            if let Ok(notification) = create_notification(
                &mut tx,
                CreateNotificationParams {
                    recipient_id: post.author_id,
                    actor_id: actor.id,
                    notification_type: NotificationType::Comment,
                    post_id: Some(post_id),
                    comment_id: Some(comment.id),
                    reaction_iri: None,
                    guestbook_entry_id: None,
                },
            )
            .await
            {
                notification_info.push((notification.id, post.author_id));
            }
        }
    }

    tx.commit().await?;

    // Send push notifications after successful commit
    if !notification_info.is_empty() {
        let push_service = state.push_service.clone();
        let db_pool = state.db_pool.clone();
        tokio::spawn(async move {
            for (notification_id, recipient_id) in notification_info {
                let mut tx = match db_pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to begin transaction for push notification: {:?}",
                            e
                        );
                        continue;
                    }
                };

                // Get the full notification with actor details
                if let Ok(Some(notification)) =
                    get_notification_by_id(&mut tx, notification_id, recipient_id).await
                {
                    // Get unread count for badge
                    let badge_count = get_unread_count(&mut tx, recipient_id)
                        .await
                        .ok()
                        .and_then(|count| u32::try_from(count).ok());

                    send_push_for_notification(&push_service, &db_pool, &notification, badge_count)
                        .await;
                }
                let _ = tx.commit().await;
            }
        });
    }

    // Return the created comment
    Ok(Json(ThreadedCommentResponse {
        id: comment.id,
        post_id: comment.post_id,
        parent_comment_id: comment.parent_comment_id,
        actor_id: comment.actor_id,
        content: comment.content,
        content_html: comment.content_html,
        actor_name: String::new(), // Will be populated by client from their cached data
        actor_handle: String::new(),
        actor_login_name: None,
        is_local: true,
        created_at: comment.created_at,
        updated_at: comment.updated_at,
        deleted_at: None,
        children: Vec::new(),
    }))
}

pub async fn delete_comment_api(
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

pub async fn delete_post_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = match auth_session.user {
        Some(u) => u,
        None => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::UNAUTHORIZED,
                    "Not authenticated",
                )),
            )
                .into_response())
        }
    };

    let post_uuid = Uuid::parse_str(&post_id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the post
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new(error_codes::NOT_FOUND, "Post not found")),
        )
            .into_response());
    }

    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if the user is the author
    let author_id = match post.get("author_id").and_then(|v| v.as_ref()) {
        Some(id) => id,
        None => {
            return Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Invalid post data",
                )),
            )
                .into_response())
        }
    };

    if author_id != &user.id.to_string() {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse::new(
                error_codes::FORBIDDEN,
                "Not authorized to delete this post",
            )),
        )
            .into_response());
    }

    // Tags are left on the post: see the note in models::hashtag::unlink.
    delete_post_with_activity(&mut tx, post_uuid, Some(&state)).await?;

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct EditPostRequest {
    pub title: String,
    pub content: String,
    pub hashtags: Option<String>,
    pub is_sensitive: bool,
    pub allow_relay: bool,
    /// Absent from a client that predates the field, which cannot have meant
    /// anything by it — the post keeps whatever it was set to.
    pub allow_replay: Option<bool>,
}

pub async fn edit_post_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    Json(request): Json<EditPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = match auth_session.user {
        Some(u) => u,
        None => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::UNAUTHORIZED,
                    "Not authenticated",
                )),
            )
                .into_response())
        }
    };

    let post_uuid = Uuid::parse_str(&post_id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the post
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new(error_codes::NOT_FOUND, "Post not found")),
        )
            .into_response());
    }

    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if the user is the author
    let author_id = match post.get("author_id").and_then(|v| v.as_ref()) {
        Some(id) => id,
        None => {
            return Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Invalid post data",
                )),
            )
                .into_response())
        }
    };

    if author_id != &user.id.to_string() {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(ErrorResponse::new(
                error_codes::FORBIDDEN,
                "Not authorized to edit this post",
            )),
        )
            .into_response());
    }

    let allow_replay = request.allow_replay.unwrap_or_else(|| {
        post.get("allow_replay")
            .and_then(|v| v.as_ref())
            .map(|v| v != "false")
            .unwrap_or(true)
    });

    // Update the post
    edit_post(
        &mut tx,
        post_uuid,
        request.title,
        request.content,
        request.is_sensitive,
        request.allow_relay,
        allow_replay,
    )
    .await?;

    // Absent leaves the tags alone, the same reading `allow_replay` above gets:
    // an edit from a client that predates the field used to unlink every tag
    // the post had and put none back.
    set_post_hashtags(&mut tx, post_uuid, request.hashtags.as_deref()).await?;

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct AddReactionRequest {
    // emoji comes from the URL path
}

#[derive(Serialize)]
pub struct ReactionResponse {
    pub reactions: Vec<ReactionCount>,
}

pub async fn add_reaction_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((post_id, emoji)): Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = match auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the user's actor
    let actor = Actor::find_by_user_id(&mut tx, user.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Check if user has already reacted with this emoji
    let existing_reaction = find_user_reaction(&mut tx, post_id, actor.id, &emoji).await?;
    if existing_reaction.is_some() {
        // Already reacted, just return current counts
        let user_actor_id = Some(actor.id);
        let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;
        tx.commit().await?;

        return Ok(Json(ReactionResponse {
            reactions: reaction_counts
                .into_iter()
                .map(|rc| ReactionCount {
                    emoji: rc.emoji,
                    count: rc.count,
                    reacted_by_user: rc.reacted_by_user,
                })
                .collect(),
        })
        .into_response());
    }

    // Create the reaction
    let reaction = create_reaction(
        &mut tx,
        ReactionDraft {
            post_id,
            actor_id: actor.id,
            emoji: emoji.clone(),
        },
        &state.config.domain,
    )
    .await?;

    // Get the post to find its author
    let post = find_post_by_id(&mut tx, post_id).await?;
    if let Some(post) = post {
        if let Some(author_id_str) = post.get("author_id").and_then(|v| v.as_ref()) {
            if let Ok(author_id) = Uuid::parse_str(author_id_str) {
                // Don't notify if reacting to own post
                if author_id != user.id {
                    // Create notification for post author
                    let notification = create_notification(
                        &mut tx,
                        CreateNotificationParams {
                            recipient_id: author_id,
                            actor_id: actor.id,
                            post_id: Some(post_id),
                            comment_id: None,
                            notification_type: NotificationType::Reaction,
                            reaction_iri: Some(reaction.iri.clone()),
                            guestbook_entry_id: None,
                        },
                    )
                    .await?;

                    // Send push notification
                    let push_service = state.push_service.clone();
                    if let Ok(Some(notification_with_actor)) =
                        get_notification_by_id(&mut tx, notification.id, author_id).await
                    {
                        let badge_count = get_unread_count(&mut tx, author_id)
                            .await
                            .ok()
                            .and_then(|count| u32::try_from(count).ok());

                        send_push_for_notification(
                            &push_service,
                            &state.db_pool,
                            &notification_with_actor,
                            badge_count,
                        )
                        .await;
                    }
                }
            }
        }
    }

    // Get updated reaction counts
    let user_actor_id = Some(actor.id);
    let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;

    tx.commit().await?;

    Ok(Json(ReactionResponse {
        reactions: reaction_counts
            .into_iter()
            .map(|rc| ReactionCount {
                emoji: rc.emoji,
                count: rc.count,
                reacted_by_user: rc.reacted_by_user,
            })
            .collect(),
    })
    .into_response())
}

pub async fn remove_reaction_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((post_id, emoji)): Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = match auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the user's actor
    let actor = Actor::find_by_user_id(&mut tx, user.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Delete the reaction
    let _ = delete_reaction(&mut tx, post_id, actor.id, &emoji).await;

    // Get updated reaction counts
    let user_actor_id = Some(actor.id);
    let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;

    tx.commit().await?;

    Ok(Json(ReactionResponse {
        reactions: reaction_counts
            .into_iter()
            .map(|rc| ReactionCount {
                emoji: rc.emoji,
                count: rc.count,
                reacted_by_user: rc.reacted_by_user,
            })
            .collect(),
    })
    .into_response())
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
    /// heading was rather than tabs in the toolbar: Recent and Popular for
    /// anyone, Following and Communities too for someone signed in.
    #[test]
    fn home_switches_between_its_feeds_in_place_of_a_heading() {
        let env = test_support::env();
        let home = env.get_template("home.jinja").expect("template loads");

        let signed_out = home
            .render(home_context(vec![sample_post()], false))
            .expect("home.jinja renders");
        assert!(signed_out.contains(r#"<a href="/" aria-current="page">feed-recent</a>"#));
        assert!(signed_out.contains(r#"<a href="/popular">feed-popular</a>"#));
        assert!(!signed_out.contains(r#"href="/following""#), "nobody's own feeds signed out");
        assert!(!signed_out.contains(r#"href="/joined""#));
        assert!(!signed_out.contains("home-section-title"), "the switch is the heading");

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
            ("popular", "/popular"),
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
        // Home is the one section for all of them, and the hashtags are a
        // section of their own.
        assert!(recent.contains(r#"<a href="/hashtags">hashtag-discovery</a>"#));
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
