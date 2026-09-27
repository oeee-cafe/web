//! Posts: the rows, and the queries on them grouped by what they serve.

use crate::models::handle::LoginName;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::types::PgInterval;
use sqlx::Type;
use uuid::Uuid;

use super::community::CommunityVisibility;

mod drafts;
pub use drafts::*;
mod profile;
pub use profile::*;
mod feeds;
pub use feeds::*;
mod urls;
pub use urls::*;
mod view;
pub use view::*;
mod edit;
pub use edit::*;
mod delete;
pub use delete::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Type, Serialize, Deserialize)]
#[sqlx(type_name = "post_deletion_reason", rename_all = "snake_case")]
pub enum PostDeletionReason {
    UserDeleted,
    Cascade,
    Moderation,
}

#[derive(Clone, Debug)]
pub struct Post {
    pub id: Uuid,
    pub image_id: Uuid,
    pub title: Option<String>,
    pub author_id: Uuid,
    pub paint_duration: PgInterval,
    pub viewer_count: i32,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]

pub struct SerializablePost {
    pub id: Uuid,
    pub title: Option<String>,
    pub author_id: Uuid,
    pub user_login_name: Option<LoginName>,
    pub paint_duration: String,
    pub stroke_count: i32,
    pub viewer_count: i32,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub replay_filename: Option<String>,
    pub is_sensitive: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct SerializableProfilePost {
    pub id: Uuid,
    pub title: Option<String>,
    pub author_id: Uuid,
    pub paint_duration: String,
    pub stroke_count: i32,
    pub viewer_count: i32,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub replay_filename: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub community_visibility: Option<CommunityVisibility>,
    pub community_slug: Option<String>,
}

#[derive(Serialize)]
pub struct SerializablePostForHome {
    pub id: Uuid,
    pub title: Option<String>,
    pub author_id: Uuid,
    pub user_login_name: LoginName,
    pub paint_duration: String,
    pub stroke_count: i32,
    pub viewer_count: i32,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub replay_filename: Option<String>,
    pub is_sensitive: bool,
    /// None for posts that belong to no community. Drives the community label
    /// on the home grid, so a card can link back to where the drawing lives.
    pub community_slug: Option<String>,
    pub community_name: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct SerializableDraftPost {
    pub id: Uuid,
    pub title: Option<String>,
    pub content: Option<String>,
    pub community_id: Option<Uuid>,
    pub community_name: Option<String>,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub updated_at: DateTime<Utc>,
}

// Minimal structs for post thumbnails (grid/list views)
#[derive(Serialize)]
pub struct PostThumbnail {
    pub id: Uuid,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
}

#[derive(Serialize)]
pub struct PostThumbnailWithSensitivity {
    pub id: Uuid,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub is_sensitive: bool,
}

#[derive(Serialize)]
pub struct SerializableThreadedPost {
    pub id: Uuid,
    pub title: Option<String>,
    pub content: Option<String>,
    pub author_id: Uuid,
    pub user_login_name: LoginName,
    pub user_display_name: String,
    pub user_actor_handle: String,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub published_at: Option<DateTime<Utc>>,
    pub published_at_formatted: Option<String>,
    pub comments_count: i64,
    /// The community the post is in, which names its page (`post_page_path`).
    pub community_slug: Option<String>,
    pub children: Vec<SerializableThreadedPost>,
}

impl Post {
    pub fn path(&self) -> String {
        format!("/posts/{}", self.id)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[sqlx(type_name = "tool", rename_all = "kebab-case")]
pub enum Tool {
    Neo,
    Tegaki,
    Cucumber,
    #[serde(rename = "neo-cucumber")]
    #[sqlx(rename = "neo-cucumber")]
    NeoCucumber,
}

pub struct PostDraft {
    pub author_id: Uuid,
    pub community_id: Option<Uuid>,
    pub paint_duration: PgInterval,
    pub stroke_count: i32,
    pub width: i32,
    pub height: i32,
    pub image_filename: String,
    pub replay_filename: Option<String>,
    pub tool: Tool,
    pub parent_post_id: Option<Uuid>,
    /// The id the browser gave the drawing when it kept it on the device;
    /// see `find_post_id_by_client_draft_id`.
    pub client_draft_id: Option<Uuid>,
}
