//! Taking a seat in a room and keeping it: the atomic join, the heartbeat,
//! and giving the seat back.

use crate::web::state::AppState;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::web::handlers::collaborate::{db, redis_messages};

use super::HEARTBEAT_INTERVAL;

/// Refreshes this connection's Redis registry entry until the socket closes.
pub(super) async fn heartbeat_loop(
    state: AppState,
    mut info: crate::web::handlers::collaborate::redis_state::ConnectionInfo,
) {
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    // The first tick completes immediately; the entry was just written.
    ticker.tick().await;

    loop {
        ticker.tick().await;

        match state
            .redis_state
            .heartbeat_connection(&info.connection_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                // The entry lapsed anyway (a Redis restart, or a beat that
                // could not be delivered in time). Re-register rather than
                // keep beating against a key that is no longer there.
                info.last_heartbeat = now_secs();
                match state.redis_state.register_connection(&info).await {
                    Ok(()) => warn!(
                        "Re-registered lapsed connection {} in room {}",
                        info.connection_id, info.room_id
                    ),
                    Err(e) => error!(
                        "Failed to re-register connection {}: {}",
                        info.connection_id, e
                    ),
                }
            }
            Err(e) => error!(
                "Heartbeat failed for connection {}: {}",
                info.connection_id, e
            ),
        }
        let store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
        if let Err(e) = store.touch_history(info.room_id).await {
            error!(
                "Failed to refresh history lifetime for room {}: {}",
                info.room_id, e
            );
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("System time is before UNIX_EPOCH")
        .as_secs()
}

/// Why a join did not go through, told apart by whether coming back could
/// help.
pub(super) enum JoinFailure {
    /// The session is over, full, or out of session user ids.
    Refused,
    /// Postgres or Redis did not answer; the same join may work in a moment.
    Unavailable,
}

pub(super) async fn setup_connection(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
    user_login_name: &str,
    connection_id: &str,
    state: &AppState,
) -> Result<
    (
        bool,
        u8,
        crate::web::handlers::collaborate::redis_state::ConnectionInfo,
    ),
    JoinFailure,
> {
    let session_info = match db::get_session_info(db, room_uuid).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            error!("Session {} not found", room_uuid);
            return Err(JoinFailure::Refused);
        }
        Err(e) => {
            error!("Failed to get session info: {}", e);
            return Err(JoinFailure::Unavailable);
        }
    };

    // Before a seat is taken: a session in a private community is for its
    // members, and a refused join must leave nothing behind.
    match db::viewer_may_enter(db, room_uuid, user_id).await {
        Ok(true) => {}
        Ok(false) => {
            info!(
                "User {} refused from session {}: not a member of its community",
                user_login_name, room_uuid
            );
            return Err(JoinFailure::Refused);
        }
        Err(e) => {
            error!("Failed to check who may enter session {}: {}", room_uuid, e);
            return Err(JoinFailure::Unavailable);
        }
    }

    // Use atomic capacity check and participant tracking to prevent race conditions
    let join_success = match db::track_participant_with_capacity_check(
        db,
        room_uuid,
        user_id,
        session_info.max_participants,
    )
    .await
    {
        Ok(success) => success,
        Err(e) => {
            error!("Failed to track participant: {}", e);
            return Err(JoinFailure::Unavailable);
        }
    };

    if !join_success {
        info!(
            "User {} rejected from session {} (capacity check failed)",
            user_login_name, room_uuid
        );
        return Err(JoinFailure::Refused);
    }

    db::update_session_activity(state, room_uuid).await;

    // The id before the registry entry, so that a join failing here has
    // nothing in Redis to undo. After the capacity check, not before it: a
    // refused joiner would otherwise burn one of the room's 255 ids.
    let session_user_id = match state.redis_state.assign_user_id(room_uuid, user_id).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            error!(
                "No session user id available for user {} in room {}",
                user_login_name, room_uuid
            );
            release_seat(db, state, room_uuid, user_id, connection_id).await;
            return Err(JoinFailure::Refused);
        }
        Err(e) => {
            error!("Failed to assign session user id: {}", e);
            release_seat(db, state, room_uuid, user_id, connection_id).await;
            return Err(JoinFailure::Unavailable);
        }
    };

    // Atomically handle all connection management
    let connection_info =
        setup_connection_atomically(state, room_uuid, user_id, connection_id, user_login_name)
            .await;

    Ok((
        session_info.owner_id == user_id,
        session_user_id,
        connection_info,
    ))
}

/// Gives back the Postgres seat a join took before it could finish. Left
/// alone, the row stays active until the session ends and counts a person who
/// never got in against the room's capacity. Not if they are in the room
/// through another socket, whose seat this is too.
async fn release_seat(
    db: &sqlx::Pool<sqlx::Postgres>,
    state: &AppState,
    room_uuid: Uuid,
    user_id: Uuid,
    connection_id: &str,
) {
    let room_connections = state
        .redis_state
        .get_room_connections(room_uuid)
        .await
        .unwrap_or_default();
    if user_has_other_connection(state, &room_connections, user_id, connection_id).await {
        return;
    }
    if let Err(e) = db::mark_participant_inactive(db, room_uuid, user_id).await {
        error!(
            "Failed to release the seat of user {} in room {}: {}",
            user_id, room_uuid, e
        );
    }
}

/// Whether this user is in the room through some other socket -- another tab,
/// or a reconnect that overlapped this one -- given the room's registry as
/// the caller already fetched it.
pub(super) async fn user_has_other_connection(
    state: &AppState,
    room_connections: &[String],
    user_id: Uuid,
    connection_id: &str,
) -> bool {
    for conn_id in room_connections {
        if conn_id == connection_id {
            continue;
        }
        if let Ok(Some(conn_info)) = state.redis_state.get_connection_info(conn_id).await
            && conn_info.user_id == user_id
        {
            return true;
        }
    }
    false
}

async fn setup_connection_atomically(
    state: &AppState,
    room_uuid: Uuid,
    user_id: Uuid,
    connection_id: &str,
    user_login_name: &str,
) -> crate::web::handlers::collaborate::redis_state::ConnectionInfo {
    // With pure Redis Pub/Sub, we don't need local room tracking
    // Each connection is independent with its own Redis subscriber

    info!(
        "Setting up Redis Pub/Sub connection for user {} in room {}",
        user_login_name, room_uuid
    );

    // Register connection in Redis
    let connection_info = crate::web::handlers::collaborate::redis_state::ConnectionInfo {
        connection_id: connection_id.to_string(),
        user_id,
        room_id: room_uuid,
        user_login_name: user_login_name.to_string(),
        server_instance: state.redis_state.get_server_instance_id().to_string(),
        connected_at: now_secs(),
        last_heartbeat: now_secs(),
    };

    if let Err(e) = state
        .redis_state
        .register_connection(&connection_info)
        .await
    {
        error!("Failed to register connection in Redis: {}", e);
    }

    // Note: Previously added user to Redis room presence, but now using database
    // for canonical participant ordering via collaborative_sessions_participants table

    info!(
        "Completed Redis Pub/Sub setup for connection {} in room {}",
        connection_id, room_uuid
    );

    connection_info
}
