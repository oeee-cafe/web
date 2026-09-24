use crate::web::state::AppState;
use anyhow::Result;
use aws_sdk_s3;
use data_encoding;
use hex;
use sha256;
use sqlx::{Pool, Postgres};
use uuid::Uuid;

pub struct SessionInfo {
    pub owner_id: Uuid,
    pub width: i32,
    pub height: i32,
    pub title: Option<String>,
    pub max_participants: i32,
}

pub async fn get_session_info(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<Option<SessionInfo>, sqlx::Error> {
    let session = sqlx::query!(
        r#"
        SELECT owner_id, width, height, title, max_participants FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        "#,
        room_uuid
    )
    .fetch_optional(db)
    .await?;

    Ok(session.map(|s| SessionInfo {
        owner_id: s.owner_id,
        width: s.width,
        height: s.height,
        title: s.title,
        max_participants: s.max_participants,
    }))
}

/// Whether this user may be in the room at all.
///
/// A session in a private community is for that community's members, the
/// way its posts are: the lobby and the preview already hide it from
/// everybody else, but the join and the metadata took anyone holding the
/// link. The owner is let in regardless, since they had to be a member to
/// open it there. A public or unlisted community, or none, is open to the
/// link.
pub async fn viewer_may_enter(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM collaborative_sessions cs
            LEFT JOIN communities c ON cs.community_id = c.id
            WHERE cs.id = $1
              AND (
                c.id IS NULL
                OR c.visibility <> 'private'
                OR cs.owner_id = $2
                OR EXISTS(
                    SELECT 1 FROM community_members cm
                    WHERE cm.community_id = c.id AND cm.user_id = $2
                )
              )
        ) AS "may_enter!"
        "#,
        room_uuid,
        user_id,
    )
    .fetch_one(db)
    .await
}

pub async fn check_existing_participant(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let existing_participant = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_sessions_participants 
            WHERE session_id = $1 AND user_id = $2
        )
        "#,
        room_uuid,
        user_id
    )
    .fetch_one(db)
    .await?
    .unwrap_or(false); // Only unwrap the Option<bool>, not the Result

    Ok(existing_participant)
}

pub async fn get_active_user_count(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<i64, sqlx::Error> {
    let active_user_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(DISTINCT user_id) as "count!"
        FROM collaborative_sessions_participants
        WHERE session_id = $1 AND is_active = true
        "#,
        room_uuid
    )
    .fetch_one(db)
    .await?; // Propagate database errors instead of returning 0

    Ok(active_user_count)
}

pub async fn track_participant(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants 
        (session_id, user_id, is_active)
        VALUES ($1, $2, true)
        ON CONFLICT (session_id, user_id) 
        DO UPDATE SET is_active = true, left_at = NULL
        "#,
        room_uuid,
        user_id
    )
    .execute(db)
    .await?;

    Ok(())
}

pub async fn track_participant_with_capacity_check(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
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

    // Check if user is already a participant (existing participants can always rejoin)
    let existing_participant = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_sessions_participants 
            WHERE session_id = $1 AND user_id = $2
        )
        "#,
        room_uuid,
        user_id
    )
    .fetch_one(&mut *tx)
    .await?
    .unwrap_or(false); // Only unwrap the Option<bool>, not the Result

    if !existing_participant {
        // For new participants, check capacity
        let active_user_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(DISTINCT user_id) as "count!"
            FROM collaborative_sessions_participants
            WHERE session_id = $1 AND is_active = true
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

    // Add or reactivate the participant
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants 
        (session_id, user_id, is_active)
        VALUES ($1, $2, true)
        ON CONFLICT (session_id, user_id) 
        DO UPDATE SET is_active = true, left_at = NULL
        "#,
        room_uuid,
        user_id
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
    user_uuid: Uuid,
    timestamp: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions_participants 
        (session_id, user_id, joined_at, is_active)
        VALUES ($1, $2, to_timestamp($3::bigint / 1000), true)
        ON CONFLICT (session_id, user_id) 
        DO UPDATE SET is_active = true, left_at = NULL
        "#,
        room_uuid,
        user_uuid,
        timestamp
    )
    .execute(db)
    .await?;

    Ok(())
}

pub async fn get_active_participants(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<Vec<crate::models::user::User>, sqlx::Error> {
    let participants = sqlx::query!(
        r#"
        SELECT csp.user_id, u.login_name FROM collaborative_sessions_participants csp
        JOIN users u ON csp.user_id = u.id
        WHERE csp.session_id = $1 AND csp.is_active = true
        ORDER BY csp.joined_at ASC
        "#,
        room_uuid
    )
    .fetch_all(db)
    .await?;

    Ok(participants
        .into_iter()
        .map(|p| crate::models::user::User {
            id: p.user_id,
            login_name: p.login_name,
            password_hash: None,
            display_name: String::new(),
            email: None,
            email_verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            banner_id: None,
            preferred_language: None,
            deleted_at: None,
            show_sensitive_content: false,
            role: crate::models::user::UserRole::User,
        })
        .collect())
}

/// Where the session's saved post lives, once it has one: the path the
/// owner's client is sent to after saving, built here from the same two
/// columns rather than taken from the client.
pub async fn saved_post_path(
    db: &Pool<Postgres>,
    room_uuid: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT cs.saved_post_id, u.login_name
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        WHERE cs.id = $1
        "#,
        room_uuid
    )
    .fetch_optional(db)
    .await?;

    Ok(row.and_then(|row| {
        row.saved_post_id
            .map(|post_id| crate::models::post::post_page_path(&row.login_name, post_id))
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
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE collaborative_sessions_participants 
        SET is_active = false, left_at = NOW()
        WHERE session_id = $1 AND user_id = $2
        "#,
        room_uuid,
        user_id
    )
    .execute(db)
    .await?;

    Ok(())
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
        SELECT u.login_name
        FROM collaborative_sessions_participants csp
        JOIN users u ON csp.user_id = u.id
        WHERE csp.session_id = $1
        ORDER BY csp.joined_at ASC
        "#,
        session_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let participant_names: Vec<String> =
        participants.iter().map(|p| p.login_name.clone()).collect();

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
