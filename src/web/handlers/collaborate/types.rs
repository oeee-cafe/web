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
    /// Whether people without an account may come in. On unless the owner
    /// turned it off.
    pub allow_guests: bool,
    /// Whether people may watch without drawing. On unless the owner turned
    /// it off.
    pub allow_spectators: bool,
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
    /// The owner's login name, or the name a guest owner gave.
    pub owner_login_name: String,
    /// A guest owner has no profile for the card to link to.
    pub owner_is_guest: bool,
    /// Whether somebody without an account may come in: the owner allows
    /// guests and the community, if any, is public. A card that says no
    /// spares a signed-out visitor a click that ends at the sign-in page.
    pub guests_welcome: bool,
    /// Whether the session can be watched: what a full card offers instead
    /// of a seat.
    pub spectators_welcome: bool,
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

/// Who the collaborative page is talking to. A browser that is neither
/// signed in nor a guest yet has no `user_id` and is asked for a name.
#[derive(Serialize)]
pub struct AuthInfo {
    /// The participant id: an account's, or the guest's.
    pub user_id: Option<String>,
    /// The account's login name, or the guest's chosen name.
    pub login_name: Option<String>,
    pub is_guest: bool,
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
    /// The saved post's page, from `db::saved_post_path` — under its
    /// community's slug when it has one, which the client cannot know.
    #[serde(rename = "savedPostUrl")]
    pub saved_post_url: Option<String>,
    #[serde(rename = "ownerLoginName")]
    pub owner_login_name: String,
    /// A guest opened it. Such a session cannot become a post until its owner
    /// has an account; until then the owner keeps it on their device.
    #[serde(rename = "ownerIsGuest")]
    pub owner_is_guest: bool,
    /// Whether somebody without an account may come in -- the owner's choice
    /// and the community's visibility together, as `viewer_may_enter` reads
    /// them.
    #[serde(rename = "guestsWelcome")]
    pub guests_welcome: bool,
    /// The owner's switch on its own, which they may flip mid-session.
    #[serde(rename = "allowGuests")]
    pub allow_guests: bool,
    /// The owner's switch for watching without drawing.
    #[serde(rename = "allowSpectators")]
    pub allow_spectators: bool,
    /// Whether the community would let guests in at all: none, or a public
    /// one. Where it would not, the owner's switch changes nothing, and the
    /// page does not offer it.
    #[serde(rename = "communityAdmitsGuests")]
    pub community_admits_guests: bool,
    /// The owner gave it no title, and `title` is a stand-in.
    pub untitled: bool,
    #[serde(rename = "maxUsers")]
    pub max_users: i32,
    #[serde(rename = "currentUserCount")]
    pub current_user_count: i64,
    /// Over, saved or not. The page shows the session-over dialog rather
    /// than opening a socket the server would refuse.
    pub ended: bool,
}
