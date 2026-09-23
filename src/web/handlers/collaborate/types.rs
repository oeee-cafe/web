use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub title: Option<String>,
    pub width: i32,
    pub height: i32,
    pub is_public: bool,
    pub max_participants: i32,
    pub community_id: Option<String>,
}

#[derive(Serialize)]
pub struct SaveSessionResponse {
    pub post_id: String,
    pub owner_login_name: String,
    pub post_url: String,
}

#[derive(Serialize)]
pub struct SessionWithCounts {
    pub id: Uuid,
    pub owner_login_name: String,
    pub title: Option<String>,
    pub width: i32,
    pub height: i32,
    pub created_at: chrono::NaiveDateTime,
    pub participant_count: i64,
    /// Seats total, so a lobby card can say "2 / 4" rather than a count with
    /// nothing to compare it against.
    pub max_participants: i32,
    /// Every seat taken. Full sessions are still listed — they are joinable by
    /// anyone already in them, and a canvas that disappears at capacity reads
    /// as one that ended.
    pub is_full: bool,
    /// False means link-only: it appears in the owner's own list, never the
    /// public one.
    pub is_public: bool,
    /// Where a saved session lands. None is a personal drawing.
    pub community_name: Option<String>,
    pub community_slug: Option<String>,
    pub is_owner: bool,
    pub viewer_is_participant: bool,
    /// When this session's preview was last uploaded, in epoch milliseconds,
    /// or None where no participant has uploaded one yet.
    ///
    /// A card with None shows no image at all rather than a broken one, and
    /// the value doubles as the `?v=` that gets a refreshed preview past the
    /// browser cache. Filled in from Redis after the query -- the previews
    /// live there, not in the row.
    pub preview_version: Option<u64>,
}

#[derive(Serialize)]
pub struct AuthInfo {
    pub user_id: String,
    pub login_name: String,
    pub preferred_locale: String,
}

#[derive(Serialize)]
pub struct CollaborationMeta {
    pub title: String,
    pub width: i32,
    pub height: i32,
    #[serde(rename = "ownerId")]
    pub owner_id: String,
    #[serde(rename = "savedPostId")]
    pub saved_post_id: Option<String>,
    #[serde(rename = "ownerLoginName")]
    pub owner_login_name: String,
    #[serde(rename = "maxUsers")]
    pub max_users: i32,
    #[serde(rename = "currentUserCount")]
    pub current_user_count: i64,
}
