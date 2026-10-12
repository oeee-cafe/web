//! Somebody in a collaborative session without an account.
//!
//! A guest is a name they chose and an id their browser session holds -- the
//! tower-sessions record every visitor already has -- with a row in
//! `collaborative_guests` behind it. Everywhere past the door a person in a
//! room is a [`Collaborator`]: one id (an account's or a guest's, the two are
//! random UUIDs from the same space), one name, and whether that name is an
//! account's. The id is what the wire protocol carries, what Redis keeps the
//! room's seats under, and the `participant_id` a row is unique by.
//!
//! A guest is never a row in `users`. An account has a profile page, is an
//! ActivityPub actor and is found by search; a guest is none of those, and
//! keeping them out of that table keeps every one of those queries honest
//! without a filter in each.

use anyhow::Context;
use sqlx::{Pool, Postgres};
use tower_sessions::Session;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::user::User;
use crate::web::state::AppState;

/// Where the browser session keeps its guest's id.
const GUEST_KEY: &str = "collaborate.guest";

/// The longest name a guest may give. Login names go to fifty, but those are
/// typed once and shown on a profile; this one is on a cursor label.
pub const NICKNAME_MAX_CHARS: usize = 30;

/// The flags byte the server puts beside every name it sends a room (JOIN,
/// CHAT, LEAVE, each LAYERS entry). The client draws a badge for it.
///
/// A flag rather than anything in the name: a guest may call themselves
/// whatever an account is called, and the room has to be able to tell them
/// apart from the account all the same.
pub const FLAG_GUEST: u8 = 0b0000_0001;

/// Somebody in a room: an account, or a guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Collaborator {
    User { id: Uuid, login_name: String },
    Guest { id: Uuid, nickname: String },
}

impl Collaborator {
    pub fn from_user(user: &User) -> Self {
        Collaborator::User {
            id: user.id,
            login_name: user.login_name.to_string(),
        }
    }

    /// The participant id: the account's, or the guest's.
    pub fn id(&self) -> Uuid {
        match self {
            Collaborator::User { id, .. } | Collaborator::Guest { id, .. } => *id,
        }
    }

    /// What the room calls them.
    pub fn name(&self) -> &str {
        match self {
            Collaborator::User { login_name, .. } => login_name,
            Collaborator::Guest { nickname, .. } => nickname,
        }
    }

    pub fn is_guest(&self) -> bool {
        matches!(self, Collaborator::Guest { .. })
    }

    pub fn user_id(&self) -> Option<Uuid> {
        match self {
            Collaborator::User { id, .. } => Some(*id),
            Collaborator::Guest { .. } => None,
        }
    }

    pub fn guest_id(&self) -> Option<Uuid> {
        match self {
            Collaborator::User { .. } => None,
            Collaborator::Guest { id, .. } => Some(*id),
        }
    }

    /// The flags byte that goes beside their name on the wire.
    pub fn flags(&self) -> u8 {
        if self.is_guest() {
            FLAG_GUEST
        } else {
            0
        }
    }
}

/// Characters that change how text around them is shown without being seen
/// themselves: bidi overrides, which can make a name read backwards into
/// somebody else's, and the zero-width ones, which make two names that look
/// the same into different strings.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// A guest's name as it will be kept, or None when there is nothing to keep.
/// Runs of whitespace become one space and the ends are trimmed; control and
/// invisible formatting characters are refused rather than stripped, since a
/// name that needed them is not the name its owner typed.
pub fn normalize_nickname(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = collapsed.chars().count();
    if length == 0 || length > NICKNAME_MAX_CHARS {
        return None;
    }
    if collapsed
        .chars()
        .any(|c| c.is_control() || is_invisible_format(c))
    {
        return None;
    }
    Some(collapsed)
}

/// The guest this browser session is, if it has been one and nobody has
/// signed in on it since.
pub async fn session_guest_id(session: &Session) -> Option<Uuid> {
    session.get::<Uuid>(GUEST_KEY).await.ok().flatten()
}

async fn find_guest(db: &Pool<Postgres>, guest_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT nickname FROM collaborative_guests WHERE id = $1 AND claimed_by IS NULL",
        guest_id
    )
    .fetch_optional(db)
    .await
}

/// Who is asking: the signed-in account, else the guest this browser session
/// already is, else nobody yet.
pub async fn current(
    user: Option<&User>,
    session: &Session,
    db: &Pool<Postgres>,
) -> Result<Option<Collaborator>, sqlx::Error> {
    if let Some(user) = user {
        return Ok(Some(Collaborator::from_user(user)));
    }
    let Some(guest_id) = session_guest_id(session).await else {
        return Ok(None);
    };
    Ok(find_guest(db, guest_id)
        .await?
        .map(|nickname| Collaborator::Guest {
            id: guest_id,
            nickname,
        }))
}

/// Makes this browser session a guest called `nickname`, or renames the guest
/// it already is. The rename is seen by the room the next time it is told who
/// is there.
pub async fn become_guest(
    session: &Session,
    db: &Pool<Postgres>,
    nickname: &str,
) -> Result<Collaborator, AppError> {
    let nickname = normalize_nickname(nickname)
        .ok_or_else(|| AppError::BadRequest("a name of 1 to 30 characters".to_string()))?;

    if let Some(guest_id) = session_guest_id(session).await {
        let renamed = sqlx::query!(
            "UPDATE collaborative_guests SET nickname = $2 WHERE id = $1 AND claimed_by IS NULL",
            guest_id,
            nickname,
        )
        .execute(db)
        .await?;
        if renamed.rows_affected() == 1 {
            return Ok(Collaborator::Guest {
                id: guest_id,
                nickname,
            });
        }
    }

    let guest_id = sqlx::query_scalar!(
        "INSERT INTO collaborative_guests (nickname) VALUES ($1) RETURNING id",
        nickname,
    )
    .fetch_one(db)
    .await?;
    session
        .insert(GUEST_KEY, guest_id)
        .await
        .map_err(|e| anyhow::anyhow!("the guest could not be kept in the session: {e}"))?;
    Ok(Collaborator::Guest {
        id: guest_id,
        nickname,
    })
}

/// Hands whatever this browser session did as a guest to the account that has
/// just signed in on it: their seats, the sessions they opened, their bans,
/// and the achievements those sessions earned.
///
/// Called after every sign-in, not only a new account's. Whoever signs in is
/// the person at the keyboard the guest was, and an account they already had
/// is as much theirs as one they just made. Never fails the sign-in: a guest
/// left unadopted keeps its name on the work, which is less than the person
/// wanted and nothing worse.
pub async fn adopt(state: &AppState, session: &Session, user_id: Uuid) {
    let Some(guest_id) = session_guest_id(session).await else {
        return;
    };
    // Gone whether or not what follows works: the session is an account's now.
    if let Err(e) = session.remove::<Uuid>(GUEST_KEY).await {
        warn!("Could not forget guest {guest_id} after a sign-in: {e}");
    }
    match adopt_guest(&state.db_pool, guest_id, user_id).await {
        Ok(open_rooms) => {
            for room in open_rooms {
                if let Err(e) = state.redis_state.rename_seat(room, guest_id, user_id).await {
                    warn!("Could not move guest {guest_id}'s seat in room {room}: {e}");
                }
            }
        }
        Err(e) => error!("Could not hand guest {guest_id} to user {user_id}: {e:#}"),
    }
}

/// [`adopt`]'s database half. Returns the rooms still open that the guest had
/// a seat in or owned, whose Redis seats are to follow.
pub(crate) async fn adopt_guest(
    db: &Pool<Postgres>,
    guest_id: Uuid,
    user_id: Uuid,
) -> anyhow::Result<Vec<Uuid>> {
    let mut tx = db.begin().await?;

    let claimed = sqlx::query!(
        r#"
        UPDATE collaborative_guests SET claimed_by = $2, claimed_at = NOW()
        WHERE id = $1 AND claimed_by IS NULL
        "#,
        guest_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(Vec::new());
    }

    // Where the account already has a seat of its own, that seat is the one
    // kept: one person, one seat. It takes the earlier of the two arrivals.
    sqlx::query!(
        r#"
        UPDATE collaborative_sessions_participants account
        SET joined_at = LEAST(account.joined_at, guest.joined_at),
            is_active = account.is_active OR guest.is_active
        FROM collaborative_sessions_participants guest
        WHERE guest.guest_id = $1 AND account.user_id = $2
          AND account.session_id = guest.session_id
        "#,
        guest_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        DELETE FROM collaborative_sessions_participants guest
        USING collaborative_sessions_participants account
        WHERE guest.guest_id = $1 AND account.user_id = $2
          AND account.session_id = guest.session_id
        "#,
        guest_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;
    let seated = sqlx::query_scalar!(
        r#"
        UPDATE collaborative_sessions_participants SET user_id = $2, guest_id = NULL
        WHERE guest_id = $1
        RETURNING session_id
        "#,
        guest_id,
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let owned = sqlx::query_scalar!(
        r#"
        UPDATE collaborative_sessions SET owner_id = $2, owner_guest_id = NULL
        WHERE owner_guest_id = $1
        RETURNING id
        "#,
        guest_id,
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    // A guest sent out of a room is not let back into it by signing in.
    sqlx::query!(
        r#"
        DELETE FROM collaborative_session_bans guest
        USING collaborative_session_bans account
        WHERE guest.participant_id = $1 AND account.participant_id = $2
          AND account.session_id = guest.session_id
        "#,
        guest_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE collaborative_session_bans SET participant_id = $2 WHERE participant_id = $1",
        guest_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;

    // FIRST_COLLABORATION, if one of their rooms was saved while they were a
    // guest. Worked out from the rows just moved, like every achievement.
    crate::models::achievement::award_achievements(&mut tx, user_id)
        .await
        .context("awarding the guest's achievements")?;

    let mut rooms: Vec<Uuid> = seated.into_iter().chain(owned).collect();
    rooms.sort();
    rooms.dedup();
    let open_rooms = sqlx::query_scalar!(
        "SELECT id FROM collaborative_sessions WHERE id = ANY($1) AND ended_at IS NULL",
        &rooms,
    )
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;
    info!(
        "Guest {guest_id} is user {user_id} now: {} rooms, {} still open",
        rooms.len(),
        open_rooms.len()
    );
    Ok(open_rooms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nickname_is_trimmed_and_its_spaces_collapsed() {
        assert_eq!(normalize_nickname("  miro  "), Some("miro".to_string()));
        assert_eq!(normalize_nickname("a \t\n b"), Some("a b".to_string()));
        assert_eq!(
            normalize_nickname("그림 그리는 사람"),
            Some("그림 그리는 사람".to_string())
        );
    }

    #[test]
    fn a_nickname_is_one_to_thirty_characters_not_bytes() {
        assert_eq!(normalize_nickname(""), None);
        assert_eq!(normalize_nickname("   "), None);
        let thirty = "가".repeat(30);
        assert_eq!(normalize_nickname(&thirty), Some(thirty.clone()));
        assert_eq!(normalize_nickname(&"가".repeat(31)), None);
    }

    #[test]
    fn a_nickname_cannot_hide_anything() {
        for hidden in [
            "mi\u{202E}ro",
            "mi\u{200B}ro",
            "mi\u{0007}ro",
            "\u{FEFF}miro",
        ] {
            assert_eq!(normalize_nickname(hidden), None, "{hidden:?}");
        }
    }

    #[test]
    fn only_a_guest_carries_the_guest_flag() {
        let guest = Collaborator::Guest {
            id: Uuid::from_u128(1),
            nickname: "miro".to_string(),
        };
        let user = Collaborator::User {
            id: Uuid::from_u128(2),
            login_name: "miro".to_string(),
        };
        assert_eq!(guest.flags(), FLAG_GUEST);
        assert_eq!(user.flags(), 0);
        assert_eq!(guest.user_id(), None);
        assert_eq!(user.guest_id(), None);
    }
}
