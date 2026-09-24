use super::collaborate::preview::PreviewStore;
use super::collaborate::redis_messages;
use crate::web::state::AppState;
use anyhow;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const CLEANUP_INTERVAL_MINUTES: u64 = 5;
const INACTIVE_THRESHOLD_MINUTES: u64 = 30;

pub async fn cleanup_collaborative_sessions(state: AppState) {
    let cleanup_interval = Duration::from_secs(CLEANUP_INTERVAL_MINUTES * 60);
    let inactive_threshold = Duration::from_secs(INACTIVE_THRESHOLD_MINUTES * 60);

    info!(
        "Starting collaborative session cleanup task (interval: {}min, threshold: {}min)",
        CLEANUP_INTERVAL_MINUTES, INACTIVE_THRESHOLD_MINUTES
    );

    loop {
        tokio::time::sleep(cleanup_interval).await;

        let start_time = Instant::now();
        let mut sessions_synced = 0;
        let mut ended_sessions_cleaned = 0;
        let mut inactive_sessions_cleaned = 0;

        let db = &state.db_pool;

        // Step 1: Sync in-memory activity cache to database (batch update)
        if let Err(e) = sync_activity_to_database(&state, db, &mut sessions_synced).await {
            error!("Failed to sync activity to database: {}", e);
        }

        // Step 2: Clean up ended sessions (those with ended_at set)
        if let Err(e) = cleanup_ended_sessions(&state, db, &mut ended_sessions_cleaned).await {
            error!("Failed to clean up ended sessions: {}", e);
        }

        // Step 3: Clean up inactive sessions (no activity for threshold duration)
        if let Err(e) = cleanup_inactive_sessions(
            &state,
            db,
            inactive_threshold,
            &mut inactive_sessions_cleaned,
        )
        .await
        {
            error!("Failed to clean up inactive sessions: {}", e);
        }

        // Step 4: Cleanup stale Redis connections
        cleanup_stale_redis_connections(&state).await;

        // Step 5: Enforce history limits on active sessions
        report_history_pressure(&state).await;

        let elapsed = start_time.elapsed();
        debug!(
            "Cleanup cycle completed in {:?}: {} sessions synced, {} ended sessions cleaned, {} inactive sessions cleaned",
            elapsed, sessions_synced, ended_sessions_cleaned, inactive_sessions_cleaned
        );
    }
}

async fn sync_activity_to_database(
    state: &AppState,
    db: &sqlx::Pool<sqlx::Postgres>,
    sessions_synced: &mut i32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Get all active sessions from database
    let active_sessions =
        sqlx::query!("SELECT id, last_activity FROM collaborative_sessions WHERE ended_at IS NULL")
            .fetch_all(db)
            .await?;

    let mut updated_sessions = 0;

    for session in active_sessions {
        let session_id = session.id;

        // Get the latest activity timestamp from Redis
        match state.redis_state.get_room_activity(session_id).await {
            Ok(Some(redis_timestamp)) => {
                // Convert Redis timestamp (seconds since epoch) to database timestamp
                let redis_activity = chrono::DateTime::from_timestamp(redis_timestamp as i64, 0)
                    .ok_or_else(|| anyhow::anyhow!("Invalid timestamp from Redis"))?
                    .naive_utc();

                // Only update if Redis has a newer timestamp than database
                if redis_activity > session.last_activity {
                    match sqlx::query!(
                        "UPDATE collaborative_sessions SET last_activity = $1 WHERE id = $2",
                        redis_activity,
                        session_id
                    )
                    .execute(db)
                    .await
                    {
                        Ok(_) => {
                            updated_sessions += 1;
                            debug!(
                                "Synced activity for session {} from Redis: {} -> {}",
                                session_id, session.last_activity, redis_activity
                            );
                        }
                        Err(e) => {
                            error!(
                                "Failed to update activity for session {}: {}",
                                session_id, e
                            );
                        }
                    }
                } else {
                    debug!(
                        "Session {} activity in database ({}) is already newer than Redis ({})",
                        session_id, session.last_activity, redis_activity
                    );
                }
            }
            Ok(None) => {
                // No activity in Redis cache - this is normal for inactive sessions
                debug!(
                    "No activity cache found in Redis for session {}",
                    session_id
                );
            }
            Err(e) => {
                error!(
                    "Failed to get activity from Redis for session {}: {}",
                    session_id, e
                );
            }
        }
    }

    *sessions_synced = updated_sessions;

    if updated_sessions > 0 {
        info!(
            "Synced activity timestamps for {} sessions from Redis to database",
            updated_sessions
        );
    } else {
        debug!("No activity timestamps needed syncing from Redis to database");
    }

    Ok(())
}

async fn cleanup_ended_sessions(
    state: &AppState,
    db: &sqlx::Pool<sqlx::Postgres>,
    ended_sessions_cleaned: &mut i32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Sessions that ended recently. Every key this pass could still find has
    // a lifetime, the archive buffer's the longest, so a session older than
    // twice that has nothing left to clean; and without the bound the sweep
    // revisited every session that had ever ended, on every pass, forever,
    // each costing a round of Redis calls.
    let window_secs = (2 * super::collaborate::archive::ARCHIVE_BUFFER_TTL) as f64;
    let ended_sessions = sqlx::query!(
        "SELECT id FROM collaborative_sessions \
         WHERE ended_at IS NOT NULL AND ended_at > NOW() - make_interval(secs => $1)",
        window_secs
    )
    .fetch_all(db)
    .await?;

    for session in ended_sessions {
        let session_id = session.id;

        // Check if there are still active connections in Redis for ended sessions
        let redis_connections = match state.redis_state.get_room_connections(session_id).await {
            Ok(connections) => connections,
            Err(e) => {
                error!(
                    "Failed to get Redis connections for session {}: {}",
                    session_id, e
                );
                Vec::new()
            }
        };

        if !redis_connections.is_empty() {
            info!(
                "Ended session {} still has {} connections - sending SESSION_EXPIRED",
                session_id,
                redis_connections.len()
            );

            // Create SESSION_EXPIRED message for ended session: [0x08][UUID:16]
            let mut session_expired_msg = vec![0x08u8]; // SESSION_EXPIRED
            session_expired_msg.extend_from_slice(session_id.as_bytes()); // 16 bytes

            // Use Redis pub/sub to notify all connections
            let room_message = super::collaborate::redis_state::RoomBroadcast {
                from_connection: "system".to_string(),
                target_connection: None,
                seq: None,
                history_id: None,
                payload: session_expired_msg,
            };

            if let Err(e) = state
                .redis_state
                .publish_message(session_id, &room_message)
                .await
            {
                error!(
                    "Failed to publish SESSION_EXPIRED message for session {}: {}",
                    session_id, e
                );
            }

            // Give connections a moment to disconnect
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Remove from in-memory structures
        let mut removed_items = Vec::new();

        // The recording is sealed first: everything below reads or removes
        // the state its manifest is written from. Not forced -- this loop
        // revisits every ended session on every pass, and a room with nothing
        // buffered has already been sealed or never had anything to seal.
        super::collaborate::archive::seal_room(state, session_id, false).await;

        // Clean up Redis message history
        let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
        if let Err(e) = redis_store.cleanup_room(session_id).await {
            error!(
                "Failed to cleanup Redis message history for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_message_history");
        }

        // Clean up all Redis room state (activity, connections, presence, snapshots)
        if let Err(e) = state.redis_state.cleanup_room_state(session_id).await {
            error!(
                "Failed to cleanup Redis room state for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_room_state");
        }

        // The lobby preview. It would expire on its own and is unreachable
        // once the session has an ended_at, but a room being cleaned up is
        // exactly when there is no reason to keep it.
        let preview_store = PreviewStore::new(state.redis_pool.clone());
        if let Err(e) = preview_store.cleanup(session_id).await {
            error!(
                "Failed to cleanup the preview for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_preview");
        }

        if !removed_items.is_empty() {
            debug!(
                "Cleaned up ended session {}: removed {}",
                session_id,
                removed_items.join(", ")
            );
            *ended_sessions_cleaned += 1;
        }
    }

    if *ended_sessions_cleaned > 0 {
        info!(
            "Cleaned up {} ended sessions from memory",
            ended_sessions_cleaned
        );
    }

    Ok(())
}

async fn cleanup_inactive_sessions(
    state: &AppState,
    db: &sqlx::Pool<sqlx::Postgres>,
    inactive_threshold: Duration,
    inactive_sessions_cleaned: &mut i32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Find sessions that have been inactive for more than the threshold
    let inactive_cutoff = chrono::Utc::now().naive_utc()
        - chrono::Duration::from_std(inactive_threshold).expect("Valid duration");

    let inactive_sessions = sqlx::query!(
        r#"
        SELECT id FROM collaborative_sessions 
        WHERE ended_at IS NULL 
        AND last_activity < $1
        "#,
        inactive_cutoff
    )
    .fetch_all(db)
    .await?;

    if inactive_sessions.is_empty() {
        return Ok(());
    }

    // Each session on its own, committed as it goes: the seal below reads
    // `ended_at` through the pool, so an update held in a transaction across
    // the whole batch was invisible to it and every inactive session was
    // sealed with no end time. It also kept a connection, on a Postgres
    // shared with other apps, open across a round of object writes per
    // session.
    for session in inactive_sessions {
        let session_id = session.id;

        // Check if there are active connections in Redis for inactive sessions
        let redis_connections = match state.redis_state.get_room_connections(session_id).await {
            Ok(connections) => connections,
            Err(e) => {
                error!(
                    "Failed to get Redis connections for session {}: {}",
                    session_id, e
                );
                Vec::new()
            }
        };

        if !redis_connections.is_empty() {
            info!(
                "Session {} has been inactive for >{}min but still has {} connections - sending SESSION_EXPIRED",
                session_id, INACTIVE_THRESHOLD_MINUTES, redis_connections.len()
            );

            // Create SESSION_EXPIRED message: [0x08][UUID:16]
            let mut session_expired_msg = vec![0x08u8]; // SESSION_EXPIRED
            session_expired_msg.extend_from_slice(session_id.as_bytes()); // 16 bytes

            // Use Redis pub/sub to notify all connections
            let room_message = super::collaborate::redis_state::RoomBroadcast {
                from_connection: "system".to_string(),
                target_connection: None,
                seq: None,
                history_id: None,
                payload: session_expired_msg,
            };

            if let Err(e) = state
                .redis_state
                .publish_message(session_id, &room_message)
                .await
            {
                error!(
                    "Failed to publish SESSION_EXPIRED message for session {}: {}",
                    session_id, e
                );
            } else {
                info!(
                    "Sent SESSION_EXPIRED to {} connections in session {}",
                    redis_connections.len(),
                    session_id
                );
            }

            // Give connections a moment to process the message and disconnect
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Mark session as ended in database
        if let Err(e) = sqlx::query!(
            "UPDATE collaborative_sessions SET ended_at = NOW() WHERE id = $1",
            session_id
        )
        .execute(db)
        .await
        {
            error!("Failed to mark session {} as ended: {}", session_id, e);
            continue;
        }

        // Mark all participants as inactive
        if let Err(e) = sqlx::query!(
            r#"
            UPDATE collaborative_sessions_participants 
            SET is_active = false, left_at = NOW()
            WHERE session_id = $1 AND is_active = true
            "#,
            session_id
        )
        .execute(db)
        .await
        {
            warn!(
                "Failed to mark participants as inactive for session {}: {}",
                session_id, e
            );
        }

        // Remove from in-memory structures
        let mut removed_items = Vec::new();

        // The recording is sealed first: everything below reads or removes
        // the state its manifest is written from. Not forced -- this loop
        // revisits every ended session on every pass, and a room with nothing
        // buffered has already been sealed or never had anything to seal.
        super::collaborate::archive::seal_room(state, session_id, false).await;

        // Clean up Redis message history
        let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
        if let Err(e) = redis_store.cleanup_room(session_id).await {
            error!(
                "Failed to cleanup Redis message history for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_message_history");
        }

        // Clean up all Redis room state (activity, connections, presence, snapshots)
        if let Err(e) = state.redis_state.cleanup_room_state(session_id).await {
            error!(
                "Failed to cleanup Redis room state for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_room_state");
        }

        // The lobby preview. It would expire on its own and is unreachable
        // once the session has an ended_at, but a room being cleaned up is
        // exactly when there is no reason to keep it.
        let preview_store = PreviewStore::new(state.redis_pool.clone());
        if let Err(e) = preview_store.cleanup(session_id).await {
            error!(
                "Failed to cleanup the preview for session {}: {}",
                session_id, e
            );
        } else {
            removed_items.push("redis_preview");
        }

        info!(
            "Cleaned up inactive session {} (inactive for >{}min): removed {} from database and memory",
            session_id,
            INACTIVE_THRESHOLD_MINUTES,
            removed_items.join(", ")
        );
        *inactive_sessions_cleaned += 1;
    }

    if *inactive_sessions_cleaned > 0 {
        info!(
            "Cleaned up {} inactive sessions from database and memory",
            inactive_sessions_cleaned
        );
    }

    Ok(())
}

/// Names the rooms whose history has outgrown what a checkpoint should have
/// replaced by now.
///
/// This sweep used to trim them, which quietly destroyed the only copy of the
/// canvas those histories began from. It reports instead: a room here is a room
/// whose auto-reset is not working, and the messages it holds are the evidence.
async fn report_history_pressure(state: &AppState) {
    let mut rooms_under_pressure = 0;

    // Get all active room IDs from database since we don't have in-memory tracking
    let db = &state.db_pool;

    let room_ids: Vec<Uuid> =
        match sqlx::query!("SELECT id FROM collaborative_sessions WHERE ended_at IS NULL")
            .fetch_all(db)
            .await
        {
            Ok(sessions) => sessions.into_iter().map(|s| s.id).collect(),
            Err(e) => {
                error!("Failed to get active sessions for history cleanup: {}", e);
                return;
            }
        };

    let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());

    for room_uuid in room_ids {
        match redis_store.history_pressure(room_uuid).await {
            Ok(Some(size)) => {
                rooms_under_pressure += 1;
                error!(
                    "Room {} has gone {} bytes past its checkpoint without a new one \
                     ({} messages, {} bytes total, wall at {}): its auto-reset is not completing",
                    room_uuid,
                    size.bytes_since_reset(),
                    size.messages,
                    size.bytes,
                    redis_messages::MAX_HISTORY_BYTES
                );
            }
            Ok(None) => {}
            Err(e) => {
                error!("Failed to measure history for room {}: {}", room_uuid, e);
            }
        }
    }

    if rooms_under_pressure > 0 {
        error!(
            "{} active sessions are drawing past the point a checkpoint should have replaced",
            rooms_under_pressure
        );
    }
}

async fn cleanup_stale_redis_connections(state: &AppState) {
    let mut cleaned_connections = 0;
    let mut rooms_checked = 0;

    // Get all active room IDs from database since we don't have in-memory tracking
    let db = &state.db_pool;

    let room_ids: Vec<Uuid> =
        match sqlx::query!("SELECT id FROM collaborative_sessions WHERE ended_at IS NULL")
            .fetch_all(db)
            .await
        {
            Ok(sessions) => sessions.into_iter().map(|s| s.id).collect(),
            Err(e) => {
                error!(
                    "Failed to get active sessions for connection cleanup: {}",
                    e
                );
                return;
            }
        };

    for room_uuid in room_ids {
        rooms_checked += 1;

        // Get all Redis connections for this room
        let redis_connections = match state.redis_state.get_room_connections(room_uuid).await {
            Ok(connections) => connections,
            Err(e) => {
                error!(
                    "Failed to get Redis connections for room {}: {}",
                    room_uuid, e
                );
                continue;
            }
        };

        // Check each Redis connection to see if it's still responding to heartbeats
        for conn_id in redis_connections {
            // Try to get connection info - if it doesn't exist or is stale, clean it up
            match state.redis_state.get_connection_info(&conn_id).await {
                Ok(Some(conn_info)) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("System time is before UNIX_EPOCH")
                        .as_secs();
                    // If connection hasn't sent a heartbeat in the last 60 seconds, consider it stale
                    if now - conn_info.last_heartbeat > 60 {
                        if let Err(e) = state.redis_state.unregister_connection(&conn_id).await {
                            error!(
                                "Failed to cleanup stale Redis connection {}: {}",
                                conn_id, e
                            );
                        } else {
                            cleaned_connections += 1;
                            debug!("Cleaned up stale Redis connection {} in room {} (last heartbeat: {}s ago)", 
                                conn_id, room_uuid, now - conn_info.last_heartbeat);
                        }
                    }
                }
                Ok(None) => {
                    // Connection info not found, but connection ID exists in room set - cleanup
                    if let Err(e) = state.redis_state.unregister_connection(&conn_id).await {
                        error!(
                            "Failed to cleanup orphaned Redis connection {}: {}",
                            conn_id, e
                        );
                    } else {
                        cleaned_connections += 1;
                        debug!(
                            "Cleaned up orphaned Redis connection {} in room {}",
                            conn_id, room_uuid
                        );
                    }
                }
                Err(e) => {
                    error!("Failed to get connection info for {}: {}", conn_id, e);
                }
            }
        }
    }

    if cleaned_connections > 0 {
        info!(
            "Redis connection cleanup: removed {} stale connections across {} rooms",
            cleaned_connections, rooms_checked
        );
    } else if rooms_checked > 0 {
        debug!(
            "Redis connection cleanup: checked {} rooms, no stale connections found",
            rooms_checked
        );
    }
}
