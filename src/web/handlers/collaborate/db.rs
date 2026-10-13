use crate::web::state::AppState;
use anyhow::Result;
use aws_sdk_s3;
use data_encoding;
use hex;
use sha256;
use sqlx::{Pool, Postgres};
use uuid::Uuid;

pub struct SessionInfo {
    /// The owner's participant id: an account's, or the guest's who opened it.
    pub owner_id: Uuid,
    pub width: i32,
    pub height: i32,
    pub title: Option<String>,
    pub max_participants: i32,
    /// When it opened, in milliseconds since the epoch. `created_at` is a
    /// TIMESTAMP written by NOW() in the connection's zone, which sqlx sets
    /// to UTC, and it is read back through a connection of the same pool.
    pub opened_at: i64,
}

pub async fn get_session_info(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<Option<SessionInfo>, sqlx::Error> {
    let session = sqlx::query!(
        r#"
        SELECT owner_participant_id, width, height, title, max_participants,
               (EXTRACT(EPOCH FROM created_at::timestamptz) * 1000)::bigint AS "opened_at!"
        FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        "#,
        room_uuid
    )
    .fetch_optional(db)
    .await?;

    Ok(session.map(|s| SessionInfo {
        owner_id: s.owner_participant_id,
        width: s.width,
        height: s.height,
        title: s.title,
        max_participants: s.max_participants,
        opened_at: s.opened_at,
    }))
}

/// Whether this person may be in the room at all. `viewer` is None for
/// somebody who is not even a guest yet, and is answered as for a guest:
/// that is what they would become to come in.
///
/// A session in a private community is for that community's members, the
/// way its posts are: the lobby and the preview already hide it from
/// everybody else, but the join and the metadata took anyone holding the
/// link. The owner is let in regardless, since they had to be a member to
/// open it there. A public or unlisted community, or none, is open to the
/// link.
///
/// A guest is a member of nothing, so a guest comes in only where the owner
/// left guests allowed and the community, if any, is public -- the rule
/// `/draw` keeps for a guest's painter (`draw::may_draw_in`). A guest who
/// owns the session is let in regardless, like any owner, and so is one
/// already in it: the owner can stop letting guests in mid-session, which
/// shuts the door on new ones and not on somebody whose connection dropped.
///
/// Nobody the owner banned comes back: not by their id, and not as a guest
/// from the address a banned guest was at (`ip`).
pub async fn viewer_may_enter(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    viewer: Option<&super::guest::Collaborator>,
    ip: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let participant_id = viewer.map(|viewer| viewer.id());
    let user_id = viewer.and_then(|viewer| viewer.user_id());
    let as_guest = user_id.is_none();
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM collaborative_sessions cs
            LEFT JOIN communities c ON cs.community_id = c.id
            WHERE cs.id = $1
              AND NOT EXISTS(
                SELECT 1 FROM collaborative_session_bans b
                WHERE b.session_id = cs.id
                  AND (b.participant_id = $2 OR ($4 AND b.ip IS NOT NULL AND b.ip = $5))
              )
              AND CASE WHEN $4 THEN
                cs.owner_participant_id = $2
                OR (
                  (c.id IS NULL OR c.visibility = 'public')
                  AND (
                    cs.allow_guests
                    OR EXISTS(
                      SELECT 1 FROM collaborative_sessions_participants csp
                      WHERE csp.session_id = cs.id AND csp.participant_id = $2
                    )
                  )
                )
              ELSE
                c.id IS NULL
                OR c.visibility <> 'private'
                OR cs.owner_id = $3
                OR EXISTS(
                    SELECT 1 FROM community_members cm
                    WHERE cm.community_id = c.id AND cm.user_id = $3
                )
              END
        ) AS "may_enter!"
        "#,
        room_uuid,
        participant_id,
        user_id,
        as_guest,
        ip,
    )
    .fetch_one(db)
    .await
}

/// Whether the owner banned this person from the room: by their id, or --
/// for a guest, or somebody who would come in as one -- by the address a
/// banned guest was at. The same test `viewer_may_enter` refuses on, asked
/// apart so the page can say "you were removed" rather than "not allowed".
pub async fn viewer_is_banned(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    viewer: Option<&super::guest::Collaborator>,
    ip: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let participant_id = viewer.map(|viewer| viewer.id());
    let as_guest = viewer.is_none_or(|viewer| viewer.is_guest());
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_session_bans b
            WHERE b.session_id = $1
              AND (b.participant_id = $2 OR ($3 AND b.ip IS NOT NULL AND b.ip = $4))
        ) AS "banned!"
        "#,
        room_uuid,
        participant_id,
        as_guest,
        ip,
    )
    .fetch_one(db)
    .await
}

pub async fn get_active_user_count(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<i64, sqlx::Error> {
    let active_user_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(DISTINCT participant_id) as "count!"
        FROM collaborative_sessions_participants
        WHERE session_id = $1 AND is_active = true AND NOT is_spectator
        "#,
        room_uuid
    )
    .fetch_one(db)
    .await?; // Propagate database errors instead of returning 0

    Ok(active_user_count)
}

pub async fn track_participant_with_capacity_check(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    who: &super::guest::Collaborator,
    max_participants: i32,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;

    // First, lock the session row to prevent concurrent modifications
    let _session = sqlx::query!(
        r#"
        SELECT max_participants
        FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        FOR UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(&mut *tx)
    .await?;

    // If session doesn't exist or has ended, fail
    if _session.is_none() {
        tx.rollback().await?;
        return Ok(false);
    }

    // Check if user is already a participant (existing participants can always
    // rejoin). Somebody who has only watched holds no seat to come back to.
    let existing_participant = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_sessions_participants
            WHERE session_id = $1 AND participant_id = $2 AND NOT is_spectator
        )
        "#,
        room_uuid,
        who.id()
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(false); // Only unwrap the Option<bool>, not the Result

    if !existing_participant {
        // For new participants, check capacity
        let active_user_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(DISTINCT participant_id) as "count!"
            FROM collaborative_sessions_participants
            WHERE session_id = $1 AND is_active = true AND NOT is_spectator
            "#,
            room_uuid
        )
        .fetch_one(&mut *tx)
        .await?;

        if active_user_count >= max_participants as i64 {
            tx.rollback().await?;
            return Ok(false);
        }
    }

    // Add or reactivate the participant. Somebody who was watching moves up
    // to drawing, for good: a drawer is credited on the post.
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants
        (session_id, user_id, guest_id, is_active, is_spectator)
        VALUES ($1, $2, $3, true, false)
        ON CONFLICT (session_id, participant_id)
        DO UPDATE SET is_active = true, left_at = NULL, is_spectator = false
        "#,
        room_uuid,
        who.user_id(),
        who.guest_id(),
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

/// How many may watch a session at once. Watchers take no drawing seat, but
/// each is a socket the room's every stroke is written to, so there is a
/// number past which a popular room would cost the server more than it
/// should for people who are only looking.
pub const MAX_SPECTATORS: i64 = 50;

/// Takes a place watching the room, where the owner allows watching and
/// fewer than [`MAX_SPECTATORS`] already are. Somebody already in the room
/// -- watching, or a drawer who has a seat -- is let back without asking
/// either, the way the guest switch lets back a guest already inside: the
/// owner closing the door shuts out new people, not somebody whose
/// connection dropped. A drawer who watches keeps their seat and their row
/// as a drawer's.
pub async fn track_spectator(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    who: &super::guest::Collaborator,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;

    let Some(session) = sqlx::query!(
        r#"
        SELECT allow_spectators
        FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        FOR UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.rollback().await?;
        return Ok(false);
    };

    let already_in = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_sessions_participants
            WHERE session_id = $1 AND participant_id = $2
        ) AS "already_in!"
        "#,
        room_uuid,
        who.id()
    )
    .fetch_one(&mut *tx)
    .await?;

    if !already_in {
        if !session.allow_spectators {
            tx.rollback().await?;
            return Ok(false);
        }
        let watching = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM collaborative_sessions_participants
            WHERE session_id = $1 AND is_active AND is_spectator
            "#,
            room_uuid
        )
        .fetch_one(&mut *tx)
        .await?;
        if watching >= MAX_SPECTATORS {
            tx.rollback().await?;
            return Ok(false);
        }
    }

    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants
        (session_id, user_id, guest_id, is_active, is_spectator)
        VALUES ($1, $2, $3, true, true)
        ON CONFLICT (session_id, participant_id)
        DO UPDATE SET is_active = true, left_at = NULL
        "#,
        room_uuid,
        who.user_id(),
        who.guest_id(),
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

pub async fn update_session_activity(state: &AppState, room_uuid: Uuid) {
    if let Err(e) = state.redis_state.update_room_activity(room_uuid).await {
        tracing::error!("Failed to update room activity in Redis: {}", e);
    }
}

pub async fn track_join_participant(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    who: &super::guest::Collaborator,
    timestamp: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants
        (session_id, user_id, guest_id, joined_at, is_active)
        VALUES ($1, $2, $3, to_timestamp($4::bigint / 1000), true)
        ON CONFLICT (session_id, participant_id)
        DO UPDATE SET is_active = true, left_at = NULL
        "#,
        room_uuid,
        who.user_id(),
        who.guest_id(),
        timestamp
    )
    .execute(db)
    .await?;

    Ok(())
}

/// Where the session's saved post lives, once it has one: the path the
/// owner's client is sent to after saving, built here from the same two
/// columns rather than taken from the client. The post's author is the
/// account that saved it, which a guest owner has to have become first.
pub async fn saved_post_path(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT p.id AS post_id, u.login_name, c.slug AS "community_slug?"
        FROM collaborative_sessions cs
        JOIN posts p ON p.id = cs.saved_post_id
        JOIN users u ON u.id = p.author_id
        LEFT JOIN communities c ON c.id = p.community_id
        WHERE cs.id = $1
        "#,
        room_uuid
    )
    .fetch_optional(db)
    .await?;

    Ok(row.map(|row| {
        crate::models::post::post_page_path(
            &row.login_name,
            row.community_slug.as_deref(),
            row.post_id,
        )
    }))
}

pub async fn end_session(db: &Pool<Postgres>, room_uuid: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE collaborative_sessions SET ended_at = NOW() WHERE id = $1 AND ended_at IS NULL",
        room_uuid
    )
    .execute(db)
    .await?;

    Ok(())
}

pub async fn mark_participant_inactive(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    participant_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE collaborative_sessions_participants
        SET is_active = false, left_at = NOW()
        WHERE session_id = $1 AND participant_id = $2
        "#,
        room_uuid,
        participant_id
    )
    .execute(db)
    .await?;

    Ok(())
}

/// Bans somebody from a room: kept out of it for as long as it lasts, and,
/// for a guest, every guest arriving from `ip` with them. Their seat is let
/// go here; their sockets are the caller's to close. Answers whether they had
/// ever been in the room -- a ban for somebody who never was is the owner
/// guessing at ids, and is not kept.
pub async fn ban_participant(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    participant_id: Uuid,
    ip: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let mut tx = db.begin().await?;
    let was_here = sqlx::query!(
        r#"
        UPDATE collaborative_sessions_participants
        SET is_active = false, left_at = COALESCE(left_at, NOW())
        WHERE session_id = $1 AND participant_id = $2
        "#,
        room_uuid,
        participant_id,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if !was_here {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query!(
        r#"
        INSERT INTO collaborative_session_bans (session_id, participant_id, ip)
        VALUES ($1, $2, $3)
        ON CONFLICT (session_id, participant_id)
        DO UPDATE SET ip = COALESCE(EXCLUDED.ip, collaborative_session_bans.ip)
        "#,
        room_uuid,
        participant_id,
        ip,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn save_session_to_post(
    db: Pool<Postgres>,
    session_id: Uuid,
    owner_id: Uuid,
    png_data: Vec<u8>,
    state: AppState,
) -> Result<(Uuid, String), Box<dyn std::error::Error + Send + Sync>> {
    // The image goes up before the row is locked. Its key is its own hash,
    // so a put is idempotent and an upload nobody then references is at
    // worst an unreferenced object -- where a put inside the transaction
    // held the session row, and with it every join to the room, for the
    // length of a network upload.
    let image_sha256 = sha256::digest(&png_data);

    let s3_client = super::archive::s3_client(&state.config);

    // SHA256 is always 64 hex characters, but let's be safe about accessing them
    let s3_key = if image_sha256.len() >= 2 {
        format!(
            "image/{}{}/{}.png",
            &image_sha256[0..1],
            &image_sha256[1..2],
            image_sha256
        )
    } else {
        // This should never happen with valid SHA256, but handle gracefully
        return Err("Invalid SHA256 hash: too short".into());
    };

    s3_client
        .put_object()
        .bucket(&state.config.aws_s3_bucket)
        .key(&s3_key)
        .content_type(crate::image_store::PNG)
        .checksum_sha256(data_encoding::BASE64.encode(&hex::decode(&image_sha256)?))
        .body(aws_sdk_s3::primitives::ByteStream::from(png_data))
        .send()
        .await?;

    let mut tx = db.begin().await?;

    // Lock the session row and check if it's already saved atomically
    let session = sqlx::query!(
        r#"
        SELECT cs.owner_id, cs.title, cs.width, cs.height, cs.community_id, 
               cs.created_at, cs.ended_at, cs.saved_post_id,
               u.login_name as owner_login_name 
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        WHERE cs.id = $1 AND cs.owner_id = $2
        FOR UPDATE
        "#,
        session_id,
        owner_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or("Session not found or not owned by user")?;

    // Check if already saved while holding the lock
    if session.saved_post_id.is_some() {
        tx.rollback().await?;
        return Err("Session has already been saved".into());
    }

    let participants = sqlx::query!(
        r#"
        SELECT COALESCE(u.login_name, g.nickname) AS "name!"
        FROM collaborative_sessions_participants csp
        LEFT JOIN users u ON csp.user_id = u.id
        LEFT JOIN collaborative_guests g ON csp.guest_id = g.id
        WHERE csp.session_id = $1 AND NOT csp.is_spectator
        ORDER BY csp.joined_at ASC
        "#,
        session_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let participant_names: Vec<String> = participants.iter().map(|p| p.name.clone()).collect();

    // The post's body: who drew it. The title is the session's own.
    let description = if participant_names.len() > 1 {
        format!(
            "Collaborative drawing with {} participants: {}",
            participant_names.len(),
            participant_names.join(", ")
        )
    } else {
        "Collaborative drawing".to_string()
    };
    let title = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string);

    let community_id = session.community_id;

    let now = chrono::Utc::now();
    let created_at_utc = session.created_at.and_utc();
    let duration = now - created_at_utc;

    let total_microseconds = duration.num_microseconds().unwrap_or(0);
    let days = duration.num_days();
    let microseconds_per_day = 24 * 60 * 60 * 1_000_000i64;
    let remainder_microseconds = total_microseconds - (days * microseconds_per_day);

    let paint_duration = sqlx::postgres::types::PgInterval {
        months: 0,
        days: days as i32,
        microseconds: remainder_microseconds,
    };

    let image_id = Uuid::new_v4();

    sqlx::query!(
        r#"
        INSERT INTO images (id, width, height, paint_duration, stroke_count, image_filename, replay_filename, tool)
        VALUES ($1, $2, $3, $4, 0, $5, NULL, 'neo-cucumber'::tool)
        "#,
        image_id,
        session.width,
        session.height,
        paint_duration,
        format!("{}.png", image_sha256),
    )
    .execute(&mut *tx)
    .await?;

    let post_id = Uuid::new_v4();
    sqlx::query!(
        r#"
        INSERT INTO posts (id, author_id, community_id, image_id, is_sensitive, published_at, title, content)
        VALUES ($1, $2, $3, $4, false, NOW(), $5, $6)
        "#,
        post_id,
        owner_id,
        community_id,
        image_id,
        title,
        description,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "UPDATE collaborative_sessions SET saved_post_id = $1 WHERE id = $2",
        post_id,
        session_id
    )
    .execute(&mut *tx)
    .await?;

    // Everyone who drew in it has collaborated.
    crate::models::achievement::award_achievements_for_session(&mut tx, session_id).await?;

    tx.commit().await?;

    tracing::info!(
        "Successfully saved collaborative drawing from session {} as post {}",
        session_id,
        post_id
    );

    Ok((post_id, session.owner_login_name))
}
