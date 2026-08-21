use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::PaginationMeta;

/// Nested image information (from images table)
#[derive(Serialize, Debug)]
pub struct ImageInfo {
    pub filename: String,
    pub width: i32,
    pub height: i32,
    pub tool: String,
    pub paint_duration: String,
}

/// Nested author information (from users table)
#[derive(Serialize, Debug)]
pub struct AuthorInfo {
    pub id: Uuid,
    pub login_name: String,
    pub display_name: String,
}

/// Nested community information for posts (simplified)
#[derive(Serialize, Debug)]
pub struct PostCommunityInfo {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
}

/// Nested image information for child posts (simplified, URL-based)
#[derive(Serialize, Debug)]
pub struct ChildPostImage {
    pub url: String,
    pub width: i32,
    pub height: i32,
}

/// Nested author information for child posts
#[derive(Serialize, Debug)]
pub struct ChildPostAuthor {
    pub id: Uuid,
    pub login_name: String,
    pub display_name: String,
    pub actor_handle: String,
}

/// Thumbnail representation of a post for list views
#[derive(Serialize, Debug)]
pub struct PostThumbnail {
    pub id: Uuid,
    pub image_url: String,
    pub image_width: i32,
    pub image_height: i32,
    pub is_sensitive: bool,
}

/// Response for post list endpoints
#[derive(Serialize, Debug)]
pub struct PostListResponse {
    pub posts: Vec<PostThumbnail>,
    pub pagination: PaginationMeta,
}

/// Detailed post information
#[derive(Serialize, Debug)]
pub struct PostDetailResponse {
    pub post: PostDetail,
    pub parent_post: Option<ChildPostResponse>,
    pub child_posts: Vec<ChildPostResponse>,
    pub reactions: Vec<ReactionCount>,
}

#[derive(Serialize, Debug)]
pub struct PostDetail {
    pub id: Uuid,
    pub title: Option<String>,
    pub content: Option<String>,
    pub author: AuthorInfo,
    pub viewer_count: i32,
    pub image: ImageInfo,
    pub is_sensitive: bool,
    pub allow_relay: bool,
    pub allow_replay: bool,
    pub published_at_utc: Option<String>,
    pub community: Option<PostCommunityInfo>,
    pub hashtags: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct CommentResponse {
    pub id: Uuid,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub content: String,
    pub content_html: Option<String>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_login_name: Option<String>,
    pub is_local: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, Debug)]
pub struct ChildPostResponse {
    pub id: Uuid,
    pub title: Option<String>,
    pub content: Option<String>,
    pub author: ChildPostAuthor,
    pub image: ChildPostImage,
    pub published_at: Option<DateTime<Utc>>,
    pub comments_count: i64,
    pub children: Vec<ChildPostResponse>,
}

#[derive(Serialize, Debug)]
pub struct ReactionCount {
    pub emoji: String,
    pub count: i64,
    pub reacted_by_user: bool,
}

/// Response for reactions detail endpoint
#[derive(Serialize, Debug)]
pub struct ReactionsDetailResponse {
    pub reactions: Vec<Reactor>,
}

#[derive(Serialize, Debug)]
pub struct Reactor {
    pub iri: String,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
    pub actor_name: String,
    pub actor_handle: String,
}

/// Threaded comment with recursive children
#[derive(Serialize, Debug)]
pub struct ThreadedCommentResponse {
    pub id: Uuid,
    pub post_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub actor_id: Uuid,
    pub content: Option<String>,
    pub content_html: Option<String>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_login_name: Option<String>,
    pub is_local: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub children: Vec<ThreadedCommentResponse>,
}

/// Response for paginated comments list endpoint
#[derive(Serialize, Debug)]
pub struct CommentsListResponse {
    pub comments: Vec<ThreadedCommentResponse>,
    pub pagination: PaginationMeta,
}

/// Response for movable communities endpoint
#[derive(Serialize, Debug)]
pub struct MovableCommunitiesResponse {
    pub communities: Vec<MovableCommunity>,
}

/// Community that a post can be moved to
#[derive(Serialize, Debug)]
pub struct MovableCommunity {
    pub id: Option<Uuid>, // None for "Personal Post" option
    pub name: String,
    pub slug: Option<String>,
    pub visibility: Option<crate::models::community::CommunityVisibility>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    pub owner_login_name: Option<String>,
    pub owner_display_name: Option<String>,
    pub has_participated: Option<bool>, // None for "Personal Post" option, true/false for communities
}

/// Request body for moving a post to a community
#[derive(serde::Deserialize, Debug)]
pub struct MoveCommunityRequest {
    pub community_id: Option<Uuid>, // None to move to personal posts
}
