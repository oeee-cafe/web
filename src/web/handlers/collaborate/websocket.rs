use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::state::AppState;
use axum::extract::{
    ws::close_code, ws::CloseFrame, ws::Message, ws::WebSocket, Path, Query, State,
    WebSocketUpgrade,
};
use axum::response::Response;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::borrow::Cow;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::{db, messages, protocol, redis_messages, utils};

struct SessionContext<'a> {
    connection_id: &'a str,
    user_login_name: &'a str,
    user_id: Uuid,
    room_uuid: Uuid,
    is_owner: bool,
    /// The 1-byte id this connection draws under. Every canvas message it
    /// sends is stamped with this before it is sequenced, so the author of a
    /// mark is never the client's to claim.
    session_user_id: u8,
    db: &'a sqlx::Pool<sqlx::Postgres>,
    state: &'a AppState,
}

/// Why the outgoing task should stop, and what to tell the client on its way
/// out. A close frame with a reason lets the client decide whether to come
/// back; a socket that simply dies leaves it guessing.
struct Goodbye {
    code: u16,
    reason: Cow<'static, str>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ResumeQuery {
    history_id: Option<Uuid>,
    after_seq: Option<u64>,
}

impl ResumeQuery {
    fn position(self) -> Option<(Uuid, u64)> {
        self.history_id.zip(self.after_seq)
    }
}

// How many messages a room may add on top of its last checkpoint before the
// server asks for a new one (Drawpile's auto-reset). Keeps catch-up for late
// joiners fast: this is what a joiner has to *apply*, where
// `AUTO_RESET_THRESHOLD_BYTES` is what it has to be sent. Neither meter stands
// in for the other -- our operations run from a two-byte undo point to half a
// megabyte of pixels -- so whichever is reached first asks.
//
// Counted from the checkpoint, not from nothing: see `maybe_request_reset`.
const RESET_THRESHOLD_MESSAGES: usize = 500;

// How often a live connection refreshes its Redis registry entry. Comfortably
// inside the 30s entry TTL, so a slow beat never drops a live connection out
// of its room.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

// How often the server pings a quiet socket.
//
// A session where nobody is drawing sends nothing in either direction, and the
// path to the browser runs through a tunnel that closes idle WebSockets after
// about a hundred seconds. The socket dies, the client reconnects, and the
// person watching gets a reconnecting dialog for no reason they can see. A
// ping is the smallest thing that keeps it warm: browsers answer it themselves,
// so the pong comes back without the page knowing, and both directions stay
// busy enough to live.
const KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

// How many live messages may be waiting for one connection's socket before the
// server gives up on it.
//
// This queue used to be unbounded, which meant a client that had stopped
// draining -- a phone that went into a tunnel, a laptop that slept -- grew a
// private backlog on the server for as long as it stayed connected, and had no
// way of ever catching up with it. Drawpile does not keep a per-client queue at
// all: each client holds a position in the shared history and is handed the
// next batch only once its socket has drained.
//
// We get the same guarantee from the other end. The canonical history is in
// Redis and a client can rejoin at any position it has already reached, so a
// connection that falls this far behind is closed and told to come back, and
// its replay starts from the last position it acknowledged. Bounded memory,
// and the client loses nothing but the socket.
//
// The number is live traffic only -- history replay is written straight to the
// socket, not through here -- so it is roughly "messages a room can produce
// while one client stalls", and a room past auto-reset holds fewer than 500
// messages in total.
const OUTGOING_QUEUE_LIMIT: usize = 1024;

// An in-progress session reset upload from this connection: `count` snapshot
// messages follow a RESET_BEGIN and replace all history up to `base_seq`.
struct PendingReset {
    base_seq: u64,
    remaining: u16,
    payloads: Vec<Vec<u8>>,
    /// False when this connection was not the one asked for the checkpoint.
    /// The snapshots are still counted off the wire -- see `parse_reset_begin`
    /// -- and then dropped.
    accepted: bool,
}

pub async fn websocket_collaborate_handler(
    Path(room_uuid): Path<Uuid>,
    auth_session: AuthSession,
    ws: WebSocketUpgrade,
    Query(resume): Query<ResumeQuery>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("Authentication required"))?;
    Ok(ws.on_upgrade(move |socket| {
        handle_socket(socket, room_uuid, state, user.id, user.login_name, resume.position())
    }))
}

pub async fn handle_socket(
    socket: WebSocket,
    room_uuid: Uuid,
    state: AppState,
    user_id: Uuid,
    user_login_name: String,
    resume_position: Option<(Uuid, u64)>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Counts this session as live until the handler returns, so a redeploy
    // waits for it to close cleanly instead of killing it mid-stroke.
    let _socket_guard = state.shutdown.track_socket();

    let connection_id = Uuid::new_v4().to_string();

    info!(
        "New websocket connection {} (user {}) joining room {}",
        connection_id, user_login_name, room_uuid
    );

    let db = &state.db_pool;

    let (is_owner, session_user_id, connection_info) = match setup_connection(
        db,
        room_uuid,
        user_id,
        &user_login_name,
        &connection_id,
        &state,
    )
    .await
    {
        Ok(owner_info) => owner_info,
        Err(_) => {
            // The join was refused (session over, full, or out of session user
            // ids). Say so with a policy close rather than just dropping the
            // socket, so the client knows not to keep retrying.
            let _ = sender
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: Cow::from("cannot join session"),
                })))
                .await;
            return;
        }
    };

    // Tell the client its 1-byte session user id before any history arrives;
    // all its drawing messages will carry this id instead of a UUID
    let welcome = Message::Binary(vec![
        messages::MessageType::Welcome as u8,
        session_user_id,
    ]);
    if sender.send(welcome).await.is_err() {
        error!(
            "Failed to send welcome to connection {} in room {}",
            connection_id, room_uuid
        );
        return;
    }

    // Who is already here, and which id each of them draws under -- before any
    // of their strokes arrive.
    //
    // This mapping also reaches the room as a LAYERS broadcast, but that one is
    // triggered by this client's own JOIN, which it cannot send until it has
    // this socket and has begun replay. Without this a joiner spends the whole
    // of its catch-up watching marks made by session ids it has no names for.
    if let Some(layers) = messages::current_layers_message(room_uuid, &state).await {
        if sender.send(Message::Binary(layers)).await.is_err() {
            error!(
                "Failed to send the participant list to connection {} in room {}",
                connection_id, room_uuid
            );
            return;
        }
    }

    // Join the room's stream BEFORE replaying history so no message can fall
    // into the gap between history replay and the live stream. Messages
    // covered by both are deduplicated below via their sequence numbers. The
    // subscription belongs to the room rather than to this connection, so the
    // eighth person to join costs a receiver rather than a Redis connection
    // and a seventh redundant decode of every stroke.
    let room_channel = state.redis_state.get_room_channel(room_uuid);
    let mut room_listener = match state.room_fanout.subscribe(room_uuid, &room_channel).await {
        Ok(listener) => listener,
        Err(e) => {
            error!(
                "Failed to join the room stream for connection {}: {}",
                connection_id, e
            );
            return;
        }
    };

    let (redis_tx, mut redis_rx) =
        mpsc::channel::<std::sync::Arc<super::redis_state::RoomBroadcast>>(OUTGOING_QUEUE_LIMIT);
    let (close_tx, close_rx) = tokio::sync::oneshot::channel::<Goodbye>();
    // Two paths can decide this connection is over -- a client that has stopped
    // draining, and a client that sent a frame this protocol does not have --
    // and only one of them gets to say goodbye.
    let close_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(close_tx)));

    let connection_id_clone = connection_id.clone();
    let overflow_close_tx = close_tx.clone();
    let redis_task = tokio::spawn(async move {
        loop {
            match room_listener.receiver.recv().await {
                Ok(room_msg) => {
                    if !should_forward_to_connection(&room_msg, &connection_id_clone) {
                        continue;
                    }
                    match redis_tx.try_send(room_msg) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            // Do not wait for room: blocking here backs the
                            // stall up into the room's shared subscriber, and
                            // the room's other members are not the ones with
                            // the problem. Close, and let this client resume
                            // from the position it last acknowledged.
                            warn!(
                                "Connection {} is {} messages behind - closing so it can resume",
                                connection_id_clone, OUTGOING_QUEUE_LIMIT
                            );
                            send_goodbye(&overflow_close_tx, close_code::AGAIN, "too far behind");
                            break;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            debug!(
                                "Redis message channel closed for connection {}",
                                connection_id_clone
                            );
                            break;
                        }
                    }
                }
                // This connection did not read its share of the room's stream
                // in time. Same answer as its own queue overflowing: the
                // canonical history is what it is missing, and a reconnect
                // replays exactly that.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    warn!(
                        "Connection {} missed {} broadcasts - closing so it can resume",
                        connection_id_clone, missed
                    );
                    send_goodbye(&overflow_close_tx, close_code::AGAIN, "too far behind");
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("Room stream ended for connection {}", connection_id_clone);
                    break;
                }
            }
        }
    });

    // Keep this connection visible in the room registry for as long as the
    // socket is open. Without it the entry lapses after CONNECTION_TTL and the
    // room looks empty to auto-reset and to the cleanup task while people are
    // still drawing. Spawned past the early returns above so no failure path
    // leaves it running.
    let heartbeat_task = tokio::spawn(heartbeat_loop(state.clone(), connection_info));

    // Send history to new connection, remembering the highest sequence number
    // it contained so the live stream can skip messages history already covered
    let (history_identity, max_history_seq) =
        send_history_to_new_connection(
            &state, room_uuid, &mut sender, &connection_id, resume_position,
        ).await;
    send_recent_chat_to_new_connection(&state, room_uuid, &mut sender, &connection_id).await;

    info!(
        "User {} joined session {} as {}",
        user_login_name,
        room_uuid,
        if is_owner { "owner" } else { "participant" }
    );

    // Handle outgoing messages (from Redis) in a separate task
    let outgoing_shutdown = state.shutdown.clone();
    let mut outgoing_task = tokio::spawn(async move {
        let mut keepalive = tokio::time::interval_at(
            tokio::time::Instant::now() + KEEPALIVE_INTERVAL,
            KEEPALIVE_INTERVAL,
        );
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut close_rx = close_rx;
        loop {
            tokio::select! {
                biased;
                // Somebody decided this connection is over and left a reason.
                goodbye = &mut close_rx => {
                    if let Ok(goodbye) = goodbye {
                        let _ = sender
                            .send(Message::Close(Some(CloseFrame {
                                code: goodbye.code,
                                reason: goodbye.reason,
                            })))
                            .await;
                    }
                    break;
                }
                // A redeploy: say goodbye properly. A close frame lets the
                // client start reconnecting to the new process right away
                // instead of inferring the loss from a severed socket.
                _ = outgoing_shutdown.signalled() => {
                    let _ = sender
                        .send(Message::Close(Some(CloseFrame {
                            code: close_code::AWAY,
                            reason: Cow::from("server restarting"),
                        })))
                        .await;
                    break;
                }
                next = redis_rx.recv() => {
                    let Some(room_msg) = next else {
                        break;
                    };
                    let msg = match room_msg.history_id.zip(room_msg.seq) {
                        // Skip sequenced messages already delivered via history replay
                        Some((history_id, s)) if history_id == history_identity && s <= max_history_seq => continue,
                        // `wrap_sequenced` builds its own buffer, so the shared
                        // payload is only read here; the clone below is the
                        // ephemeral path, which is a pointer position at most.
                        Some((history_id, s)) => Message::Binary(wrap_sequenced(history_id, s, &room_msg.payload)),
                        None => Message::Binary(room_msg.payload.clone()),
                    };
                    if sender.send(msg).await.is_err() {
                        debug!("WebSocket send failed");
                        break;
                    }
                }
                _ = keepalive.tick() => {
                    if sender.send(Message::Ping(Vec::new())).await.is_err() {
                        debug!("WebSocket keepalive failed");
                        break;
                    }
                }
            }
        }
    });

    handle_incoming_messages(
        &mut receiver,
        SessionContext {
            connection_id: &connection_id,
            user_login_name: &user_login_name,
            user_id,
            room_uuid,
            is_owner,
            session_user_id,
            db,
            state: &state,
        },
        &close_tx,
    )
    .await;

    // Stop the heartbeat before cleaning up, or a beat landing between the
    // unregister and the abort would helpfully re-register this connection.
    heartbeat_task.abort();

    cleanup_connection(
        &connection_id,
        &user_login_name,
        user_id,
        room_uuid,
        db,
        &state,
    )
    .await;

    redis_task.abort();
    // Gives up this connection's share of the room's subscription. The task
    // above owned the receiver, so aborting it is what makes this the last
    // reference; when it is also the room's last, the Redis subscription goes
    // with it.
    state.room_fanout.release(room_uuid).await;

    // A goodbye is only a goodbye if it reaches the wire. When one has been
    // handed over, give the outgoing task a moment to send it before the abort
    // takes the socket out from under it -- a client told "come back" resumes,
    // where a client whose socket merely died has to work out what happened.
    let goodbye_pending = close_tx
        .lock()
        .expect("close channel is never held across a panic")
        .is_none();
    if goodbye_pending {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(1), &mut outgoing_task).await;
    }
    outgoing_task.abort();
}

/// Hands the outgoing task a close frame to send before it stops. Only the
/// first caller is heard: once the socket is closing, a second reason for it
/// would be a frame sent after the close.
fn send_goodbye(
    close_tx: &std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Goodbye>>>>,
    code: u16,
    reason: &'static str,
) {
    let sender = close_tx
        .lock()
        .expect("close channel is never held across a panic")
        .take();
    if let Some(sender) = sender {
        let _ = sender.send(Goodbye {
            code,
            reason: Cow::from(reason),
        });
    }
}

/// Refreshes this connection's Redis registry entry until the socket closes.
async fn heartbeat_loop(state: AppState, mut info: super::redis_state::ConnectionInfo) {
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
            error!("Failed to refresh history lifetime for room {}: {}", info.room_id, e);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("System time is before UNIX_EPOCH")
        .as_secs()
}

async fn setup_connection(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
    user_login_name: &str,
    connection_id: &str,
    state: &AppState,
) -> Result<(bool, u8, super::redis_state::ConnectionInfo), ()> {
    let session_info = match db::get_session_info(db, room_uuid).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            error!("Session {} not found", room_uuid);
            return Err(());
        }
        Err(e) => {
            error!("Failed to get session info: {}", e);
            return Err(());
        }
    };

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
            false
        }
    };

    if !join_success {
        info!(
            "User {} rejected from session {} (capacity check failed)",
            user_login_name, room_uuid
        );
        return Err(());
    }

    db::update_session_activity(state, room_uuid).await;

    // Atomically handle all connection management
    let connection_info =
        setup_connection_atomically(state, room_uuid, user_id, connection_id, user_login_name)
            .await;

    let session_user_id = match state.redis_state.assign_user_id(room_uuid, user_id).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            error!(
                "No session user id available for user {} in room {}",
                user_login_name, room_uuid
            );
            return Err(());
        }
        Err(e) => {
            error!("Failed to assign session user id: {}", e);
            return Err(());
        }
    };

    Ok((
        session_info.owner_id == user_id,
        session_user_id,
        connection_info,
    ))
}

async fn setup_connection_atomically(
    state: &AppState,
    room_uuid: Uuid,
    user_id: Uuid,
    connection_id: &str,
    user_login_name: &str,
) -> super::redis_state::ConnectionInfo {
    // With pure Redis Pub/Sub, we don't need local room tracking
    // Each connection is independent with its own Redis subscriber

    info!(
        "Setting up Redis Pub/Sub connection for user {} in room {}",
        user_login_name, room_uuid
    );

    // Register connection in Redis
    let connection_info = super::redis_state::ConnectionInfo {
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

// Wraps a history message in
// [0x0A][history UUID: 16 bytes][seq: 8 bytes LE][payload], so clients only
// compare positions that belong to the same canonical history.
pub(super) fn wrap_sequenced(history_id: Uuid, seq: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(25 + payload.len());
    buf.push(messages::MessageType::Sequenced as u8);
    buf.extend_from_slice(history_id.as_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn should_forward_to_connection(
    room_msg: &super::redis_state::RoomBroadcast,
    connection_id: &str,
) -> bool {
    // Targeted messages (e.g. RESET_REQUEST) go to exactly one connection
    if let Some(target) = &room_msg.target_connection {
        return target == connection_id;
    }
    if room_msg.from_connection != connection_id {
        return true;
    }
    // Echo every stored client drawing message back to its sender in canonical
    // server order so the client can confirm its optimistic fork. Keeping an
    // explicit list here lost newer operations (LINE was the first visible
    // casualty) whenever the protocol grew. Pointer messages are ephemeral
    // and must not enter reconciliation. Snapshot is a stored
    // server-range message; chat is echoed as delivery confirmation.
    // END_SESSION is the lifecycle transition the owner is waiting on: it is
    // published only once the session is really over, and the owner navigates
    // to the saved post on receiving it, so it must come back to its sender.
    match room_msg.payload.first().copied() {
        Some(0x02) | Some(0x03) | Some(0x07) => true,
        Some(msg_type) if messages::is_client_message(msg_type) => {
            msg_type != 0x13 && msg_type != 0x1c
        }
        _ => false,
    }
}

fn accepted_resume_sequence(
    history_id: Uuid,
    current_max_seq: u64,
    resume_position: Option<(Uuid, u64)>,
) -> u64 {
    match resume_position {
        Some((resume_history_id, resume_seq))
            if resume_history_id == history_id && resume_seq <= current_max_seq => resume_seq,
        _ => 0,
    }
}

#[cfg(test)]
mod forwarding_tests {
    use super::{accepted_resume_sequence, should_forward_to_connection};
    use crate::web::handlers::collaborate::redis_state::RoomBroadcast;
    use uuid::Uuid;

    fn message(from: &str, msg_type: u8) -> RoomBroadcast {
        RoomBroadcast {
            from_connection: from.to_string(),
            target_connection: None,
            seq: Some(1),
            history_id: Some(Uuid::nil()),
            payload: vec![msg_type],
        }
    }

    #[test]
    fn echoes_every_drawing_operation_to_its_sender() {
        for msg_type in [
            0x02, // snapshot
            0x12, // fill
            0x14, // undo point
            0x15, // undo
            0x16, // freehand stroke
            0x17, // region
            0x18, // line
            0x19, // bezier
            0x1a, // erase all
            0x1b, // text
            0x1d, // a rectangle of pixels, which is how a fill travels
        ] {
            assert!(
                should_forward_to_connection(&message("same", msg_type), "same"),
                "message 0x{msg_type:02x} was not echoed"
            );
        }
    }

    #[test]
    fn does_not_echo_ephemeral_pointer_updates_to_the_sender() {
        for msg_type in [0x13, 0x1c] {
            assert!(!should_forward_to_connection(
                &message("same", msg_type),
                "same"
            ));
        }
    }

    /// The owner ends the session and then waits for the server to say so
    /// before navigating to the saved post. Filtering the echo out strands the
    /// owner on a finished session while everyone else is redirected.
    #[test]
    fn echoes_the_end_of_the_session_to_the_owner_who_ended_it() {
        assert!(should_forward_to_connection(&message("same", 0x07), "same"));
    }

    #[test]
    fn forwards_messages_from_other_connections() {
        for msg_type in [0x13, 0x1c] {
            assert!(should_forward_to_connection(
                &message("other", msg_type),
                "same"
            ));
        }
    }

    /// Everything after a RESET_BEGIN is read as part of the checkpoint until
    /// its count runs out, so a count the server should not have believed
    /// either swallows the room's drawing or lets a checkpoint's snapshots
    /// loose into history as ordinary messages.
    #[test]
    fn believes_only_a_snapshot_count_a_checkpoint_could_have() {
        use super::reset_snapshot_count;

        let announce = |base: u64, count: u16| {
            let mut frame = vec![0x0c];
            frame.extend_from_slice(&base.to_le_bytes());
            frame.extend_from_slice(&count.to_le_bytes());
            frame
        };

        // A pair per participant, from one participant up to the whole id space.
        assert_eq!(reset_snapshot_count(&announce(7, 2)), Some((7, 2)));
        assert_eq!(reset_snapshot_count(&announce(0, 16)), Some((0, 16)));
        assert_eq!(reset_snapshot_count(&announce(1, 510)), Some((1, 510)));

        // A checkpoint of nothing, of half a participant, or of more
        // participants than a session can hold.
        assert_eq!(reset_snapshot_count(&announce(7, 0)), None);
        assert_eq!(reset_snapshot_count(&announce(7, 3)), None);
        assert_eq!(reset_snapshot_count(&announce(7, 512)), None);

        // A frame too short to hold the count at all.
        assert_eq!(reset_snapshot_count(&[0x0c, 0, 0]), None);
    }

    #[test]
    fn resumes_only_a_position_on_the_current_history() {
        let history_id = Uuid::new_v4();
        assert_eq!(accepted_resume_sequence(history_id, 12, Some((history_id, 7))), 7);
        assert_eq!(
            accepted_resume_sequence(history_id, 12, Some((Uuid::new_v4(), 7))),
            0
        );
        assert_eq!(accepted_resume_sequence(history_id, 12, Some((history_id, 13))), 0);
        assert_eq!(accepted_resume_sequence(history_id, 12, None), 0);
    }
}

// Returns the highest sequence number contained in the replayed history,
// or 0 if the history is empty or could not be retrieved.
async fn send_history_to_new_connection(
    state: &AppState,
    room_uuid: Uuid,
    sender: &mut SplitSink<WebSocket, Message>,
    connection_id: &str,
    resume_position: Option<(Uuid, u64)>,
) -> (Uuid, u64) {
    let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());

    let mut max_seq = 0;
    match redis_store.get_history_snapshot(room_uuid).await {
        Ok((history_id, history)) => {
            let current_max_seq = history.iter().map(|(seq, _)| *seq).max().unwrap_or(0);
            let after_seq = accepted_resume_sequence(
                history_id, current_max_seq, resume_position,
            );
            match resume_position {
                Some((resume_history_id, resume_seq)) if after_seq == resume_seq
                    && resume_history_id == history_id => {
                    debug!(
                        "Resuming connection {} in history {} after seq {}",
                        connection_id, history_id, resume_seq
                    );
                }
                Some(_) => {
                    debug!("Resume position rejected for {}; sending full history", connection_id);
                }
                None => {}
            }
            let mut replay_start = Vec::with_capacity(33);
            replay_start.push(messages::MessageType::ReplayStart as u8);
            replay_start.extend_from_slice(history_id.as_bytes());
            replay_start.extend_from_slice(&after_seq.to_le_bytes());
            replay_start.extend_from_slice(&current_max_seq.to_le_bytes());
            if sender.send(Message::Binary(replay_start)).await.is_err() {
                warn!("Failed to send replay boundary to {}", connection_id);
                return (history_id, after_seq);
            }
            for (seq, stored_msg) in history.iter() {
                if *seq <= after_seq {
                    max_seq = max_seq.max(*seq);
                    continue;
                }
                let payload = match stored_msg {
                    Message::Binary(data) => data.as_slice(),
                    _ => continue,
                };
                let wrapped = Message::Binary(wrap_sequenced(history_id, *seq, payload));
                // `feed` rather than `send`: a replay is up to the whole
                // auto-reset threshold of messages, and `send` flushes each one
                // on its own. The flush below covers all of them, so a join
                // costs a few writes instead of one per stored operation.
                if sender.feed(wrapped).await.is_err() {
                    warn!(
                        "Failed to send stored message to new connection {}",
                        connection_id
                    );
                    break;
                }
                max_seq = max_seq.max(*seq);
            }
            if sender.flush().await.is_err() {
                warn!("Failed to flush replayed history to {}", connection_id);
                return (history_id, max_seq);
            }
            debug!(
                "Sent {} stored messages from Redis to new connection {} (max seq {})",
                history.len(),
                connection_id,
                max_seq
            );
            let mut caught_up = Vec::with_capacity(25);
            caught_up.push(messages::MessageType::CaughtUp as u8);
            caught_up.extend_from_slice(history_id.as_bytes());
            caught_up.extend_from_slice(&max_seq.to_le_bytes());
            if sender.send(Message::Binary(caught_up)).await.is_err() {
                warn!("Failed to send caught-up marker to {}", connection_id);
            }
            return (history_id, max_seq);
        }
        Err(e) => {
            error!(
                "Failed to retrieve message history from Redis for connection {}: {}",
                connection_id, e
            );
        }
    }
    (Uuid::nil(), max_seq)
}

async fn handle_incoming_messages(
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    ctx: SessionContext<'_>,
    close_tx: &std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Goodbye>>>>,
) {
    let mut pending_reset: Option<PendingReset> = None;

    loop {
        let msg = tokio::select! {
            biased;
            // Stop reading on a redeploy so the caller's cleanup runs: without
            // it the task is dropped where it stands and nobody in the room
            // ever hears that this user left.
            _ = ctx.state.shutdown.signalled() => {
                info!(
                    "Server shutting down, closing connection {} in room {}",
                    ctx.connection_id, ctx.room_uuid
                );
                break;
            }
            msg = receiver.next() => match msg {
                Some(msg) => msg,
                None => break,
            },
        };

        let msg = match msg {
            Ok(msg) => msg,
            Err(e) => {
                error!(
                    "Websocket error for connection {}: {}",
                    ctx.connection_id, e
                );
                break;
            }
        };

        let Message::Binary(mut data) = msg else {
            continue;
        };

        // Nothing the server cannot lay out gets past here. History is
        // replayed to everyone who joins later, so a frame accepted once is
        // accepted for as long as the session lives, and a frame nobody can
        // parse is a gap in every future replay.
        match protocol::validate(&data) {
            Ok(()) => {}
            Err(rejection) if rejection.is_fatal() => {
                warn!(
                    "Closing connection {} in room {}: {}",
                    ctx.connection_id, ctx.room_uuid, rejection
                );
                send_goodbye(close_tx, close_code::INVALID, "malformed message");
                break;
            }
            Err(rejection) => {
                debug!(
                    "Ignoring message from connection {} in room {}: {}",
                    ctx.connection_id, ctx.room_uuid, rejection
                );
                continue;
            }
        }

        // The author of a mark is the server's to decide, not the sender's.
        if let Some(claimed) = protocol::enforce_origin(&mut data, ctx.session_user_id) {
            warn!(
                "Connection {} (user {}) sent a 0x{:02x} authored by session user {} in room {}; \
                 rewritten as {}",
                ctx.connection_id,
                ctx.user_login_name,
                data[0],
                claimed,
                ctx.room_uuid,
                ctx.session_user_id
            );
        }

        let mut msg = Message::Binary(data);

        if let Message::Binary(data) = &msg {
            // Session reset upload: RESET_BEGIN announces the snapshots, then
            // the snapshots are captured here — they replace history instead
            // of being sequenced or broadcast (live clients already have this
            // state; only late joiners replay the reset)
            if let Some(reset) = pending_reset.as_mut() {
                if data.first() == Some(&(messages::MessageType::Snapshot as u8)) {
                    if reset.accepted {
                        reset.payloads.push(data.clone());
                    }
                    reset.remaining -= 1;
                    if reset.remaining == 0 {
                        let reset = pending_reset.take().expect("pending reset exists");
                        if reset.accepted {
                            finish_reset(&ctx, reset).await;
                        }
                    }
                    continue;
                }
            }
            if data.first() == Some(&(messages::MessageType::ResetBegin as u8)) {
                pending_reset = parse_reset_begin(data, &ctx).await;
                continue;
            }

            // Validation above guarantees a type byte.
            let msg_type = data[0];
            if msg_type < 0x10 {
                msg = match process_server_message(msg_type, data, &msg, &ctx).await {
                    Some(processed_msg) => processed_msg,
                    None => continue,
                };
            }
        }

        if messages::should_store_message(&msg) {
            // History messages go through the atomic sequencer, which stores
            // and broadcasts them in one step so every client observes the
            // same canonical order (Drawpile-style server-side serialization)
            match messages::sequence_and_broadcast(
                &msg,
                ctx.room_uuid,
                ctx.connection_id,
                ctx.state,
            )
            .await
            {
                Ok(redis_messages::Sequenced::Stored { seq, size, .. }) => {
                    // The activity stamp and both auto-reset meters came back
                    // with the sequence number, inside the same script. They
                    // used to be three more round trips, taken on every
                    // drawing message before this loop would read the next one
                    // from the same client.
                    maybe_request_reset(&ctx, size).await;
                    // The message is already in the archive buffer; this is
                    // only the nudge that moves a batch of them into storage,
                    // and it is spawned because this task is in the middle of
                    // somebody's stroke.
                    super::archive::maybe_flush(ctx.state, ctx.room_uuid, seq);
                }
                Ok(redis_messages::Sequenced::HistoryFull { .. }) => {
                    // The room is out of room. The message is gone -- its
                    // sender will notice, because the echo it is waiting on to
                    // confirm its own stroke never arrives -- and the one thing
                    // that can help is a checkpoint, so ask for one even if an
                    // earlier request is still outstanding.
                    force_reset_request(&ctx).await;
                    continue;
                }
                Err(e) => {
                    error!(
                        "Failed to sequence message for room {}: {}",
                        ctx.room_uuid, e
                    );
                    continue;
                }
            }
        } else {
            // Ephemeral messages (chat, join, leave) bypass the sequencer
            if let Message::Binary(data) = &msg {
                if data.first() == Some(&(messages::MessageType::Chat as u8)) {
                    let store =
                        redis_messages::RedisMessageStore::new(ctx.state.redis_pool.clone());
                    if let Err(e) = store.append_chat_message(ctx.room_uuid, data).await {
                        error!("Failed to preserve recent chat in room {}: {}", ctx.room_uuid, e);
                    }
                    // And into the recording. `data` here is the frame the
                    // server built in `process_server_message`, so the name on
                    // it is the one it authenticated.
                    super::archive::record_chat(ctx.state, ctx.room_uuid, data).await;
                }
            }
            messages::broadcast_message(&msg, ctx.room_uuid, ctx.connection_id, ctx.state).await;
        }
    }

    // If this connection was mid-reset, release the flag so another client
    // can be asked without waiting for the TTL. Only if the checkpoint was
    // actually ours: a connection whose unasked-for upload we were discarding
    // would otherwise take the job away from whoever really has it.
    if pending_reset.is_some_and(|reset| reset.accepted) {
        if let Err(e) = ctx.state.redis_state.clear_reset_pending(ctx.room_uuid).await {
            error!(
                "Failed to clear reset-pending flag for room {}: {}",
                ctx.room_uuid, e
            );
        }
    }
}

async fn send_recent_chat_to_new_connection(
    state: &AppState,
    room_uuid: Uuid,
    sender: &mut SplitSink<WebSocket, Message>,
    connection_id: &str,
) {
    let store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
    match store.get_recent_chat(room_uuid).await {
        Ok(messages) => {
            for payload in &messages {
                if sender.send(Message::Binary(payload.clone())).await.is_err() {
                    warn!("Failed to replay recent chat to {}", connection_id);
                    break;
                }
            }
            debug!("Sent {} recent chat messages to {}", messages.len(), connection_id);
        }
        Err(e) => error!("Failed to load recent chat for room {}: {}", room_uuid, e),
    }
}

/// The position and snapshot count a RESET_BEGIN announces, if it announces a
/// checkpoint that could exist.
///
/// A checkpoint is one background and one foreground per participant who has
/// drawn, so the count is even, non-zero, and bounded by the session user id
/// space. The pairing itself -- which participant each snapshot belongs to --
/// is checked against the payloads once they arrive, in `valid_reset_payloads`.
///
/// None means the count cannot be trusted, and a count that cannot be trusted
/// is worse than useless: everything after a RESET_BEGIN is read as part of the
/// checkpoint until the count runs out, so a wrong one either swallows ordinary
/// drawing or lets snapshots loose into history.
pub(super) fn reset_snapshot_count(data: &[u8]) -> Option<(u64, u16)> {
    if data.len() < 11 {
        return None;
    }
    let base_seq = utils::read_u64_le(data, 1);
    let count = u16::from_le_bytes([data[9], data[10]]);
    if count == 0 || count % 2 != 0 || count > 2 * u16::from(u8::MAX) {
        return None;
    }
    Some((base_seq, count))
}

async fn parse_reset_begin(data: &[u8], ctx: &SessionContext<'_>) -> Option<PendingReset> {
    let Some((base_seq, count)) = reset_snapshot_count(data) else {
        warn!(
            "Rejecting an unusable RESET_BEGIN from connection {} in room {}",
            ctx.connection_id, ctx.room_uuid
        );
        return None;
    };
    let accepted = match ctx
        .state
        .redis_state
        .is_reset_uploader(ctx.room_uuid, ctx.connection_id)
        .await
    {
        Ok(accepted) => accepted,
        Err(e) => {
            error!(
                "Could not authorize reset uploader {}: {}",
                ctx.connection_id, e
            );
            false
        }
    };

    if accepted {
        info!(
            "Session reset upload started for room {} at seq {} ({} snapshots)",
            ctx.room_uuid, base_seq, count
        );
    } else {
        // The snapshots are coming whether we asked for them or not: a client
        // that predates the query phase reads the query as an instruction and
        // starts uploading, and a client that lost the race to answer may
        // already be under way. They are counted off the wire and dropped
        // here, because a snapshot the server does not recognise as part of a
        // checkpoint is still a valid message -- it would be sequenced into
        // history as an ordinary one and stamp the uploader's canvas over
        // everybody's.
        warn!(
            "Discarding a {}-snapshot upload from unselected connection {} in room {}",
            count, ctx.connection_id, ctx.room_uuid
        );
    }

    Some(PendingReset {
        base_seq,
        remaining: count,
        payloads: Vec::with_capacity(if accepted { count as usize } else { 0 }),
        accepted,
    })
}

async fn finish_reset(ctx: &SessionContext<'_>, reset: PendingReset) {
    if !valid_reset_payloads(&reset.payloads) {
        warn!(
            "Rejecting malformed reset snapshots from connection {} in room {}",
            ctx.connection_id, ctx.room_uuid
        );
        let _ = ctx.state.redis_state.clear_reset_pending(ctx.room_uuid).await;
        return;
    }
    let redis_store = redis_messages::RedisMessageStore::new(ctx.state.redis_pool.clone());
    match redis_store
        .apply_reset(ctx.room_uuid, reset.base_seq, &reset.payloads)
        .await
    {
        Ok(()) => {
            info!(
                "Session reset applied for room {} at seq {} ({} snapshots)",
                ctx.room_uuid,
                reset.base_seq,
                reset.payloads.len()
            );

            // Tell all clients (and future late joiners, via history) that
            // everything at or below base_seq is squashed into the reset
            // snapshots, so they can freeze undo state and reclaim memory
            // The count travels with the point because every snapshot of a
            // reset is stored at the same sequence: without it a client has no
            // way to tell a half-arrived checkpoint from a whole one, and with
            // a pair per participant there is no longer a fixed number to
            // assume.
            let mut reset_point = vec![messages::MessageType::ResetPoint as u8];
            reset_point.extend_from_slice(&reset.base_seq.to_le_bytes());
            reset_point.extend_from_slice(&(reset.payloads.len() as u16).to_le_bytes());
            if let Err(e) = messages::sequence_and_broadcast(
                &Message::Binary(reset_point),
                ctx.room_uuid,
                "system",
                ctx.state,
            )
            .await
            {
                error!(
                    "Failed to broadcast reset point for room {}: {}",
                    ctx.room_uuid, e
                );
            }
        }
        Err(e) => {
            error!(
                "Failed to apply session reset for room {}: {}",
                ctx.room_uuid, e
            );
        }
    }

    if let Err(e) = ctx.state.redis_state.clear_reset_pending(ctx.room_uuid).await {
        error!(
            "Failed to clear reset-pending flag for room {}: {}",
            ctx.room_uuid, e
        );
    }
}

/// A reset checkpoint describes every participant's layer pair, so it carries
/// one background and one foreground per participant who has drawn -- not the
/// two of a shared canvas. Each snapshot names its owner in the user byte.
pub(super) fn valid_reset_payloads(payloads: &[Vec<u8>]) -> bool {
    if payloads.is_empty() || payloads.len() % 2 != 0 {
        return false;
    }
    let mut seen: std::collections::HashMap<u8, u8> = std::collections::HashMap::new();
    for payload in payloads {
        if payload.len() < 4 || payload[0] != messages::MessageType::Snapshot as u8 {
            return false;
        }
        // [type][author][target owner][layer]...: one client uploads the whole
        // canvas, so the author is the same throughout and the owner byte is
        // what says whose pair each snapshot is.
        let (owner, layer) = (payload[2], payload[3]);
        if layer > 1 {
            return false;
        }
        let bit = 1u8 << layer;
        let held = seen.entry(owner).or_insert(0);
        // The same layer twice for one participant would leave the checkpoint
        // ambiguous about which copy is current.
        if *held & bit != 0 {
            return false;
        }
        *held |= bit;
    }
    seen.values().all(|held| *held == 0b11)
}

/// Asks the room for a checkpoint once it has drawn enough on top of the last
/// one to be worth replacing.
///
/// `size` is measured by the sequencer, in the same script that stored the
/// message it describes -- this is on the path of every drawing message, and a
/// separate read to answer a question that is almost always "no" cost the room
/// a round trip per mark.
async fn maybe_request_reset(ctx: &SessionContext<'_>, size: redis_messages::HistorySize) {
    // Two meters, both counted from the last checkpoint rather than from
    // nothing, because a checkpoint is most of what a busy room's history
    // weighs and an absolute threshold would be over the moment one landed.
    //
    // Bytes for what a late joiner has to be sent, messages for what it then
    // has to apply: our operations run from a two-byte undo point to a
    // half-megabyte region of pixels, so neither meter stands in for the other.
    let over_bytes = size.bytes > redis_messages::effective_auto_reset_bytes(size.base_bytes);
    let over_messages = size.messages_since_reset() >= RESET_THRESHOLD_MESSAGES;
    if !over_bytes && !over_messages {
        return;
    }

    match ctx.state.redis_state.try_open_reset_query(ctx.room_uuid).await {
        Ok(true) => {}
        Ok(false) => return, // a query or an upload is already in flight
        Err(e) => {
            error!(
                "Failed to open a checkpoint query for room {}: {}",
                ctx.room_uuid, e
            );
            return;
        }
    }

    info!(
        "Room {} has added {} messages and {} bytes since its last checkpoint - asking for a new one",
        ctx.room_uuid,
        size.messages_since_reset(),
        size.bytes_since_reset()
    );
    messages::send_reset_request(ctx.room_uuid, None, messages::ResetPhase::Query, ctx.state).await;
}

/// Asks again regardless of what is already outstanding, for the room that has
/// run out of history and cannot draw until somebody checkpoints it.
async fn force_reset_request(ctx: &SessionContext<'_>) {
    match ctx.state.redis_state.reopen_reset_query(ctx.room_uuid).await {
        // A query is already out and unanswered; asking again would only add
        // to what the room cannot deliver.
        Ok(false) => return,
        Ok(true) => {}
        Err(e) => {
            error!(
                "Failed to reopen the checkpoint query for room {}: {}",
                ctx.room_uuid, e
            );
            return;
        }
    }
    messages::send_reset_request(ctx.room_uuid, None, messages::ResetPhase::Query, ctx.state).await;
}

/// A client says it is caught up and able to upload the checkpoint. The first
/// one to say so gets it; the rest are told nothing and do nothing.
async fn handle_reset_offer(ctx: &SessionContext<'_>) {
    match ctx
        .state
        .redis_state
        .claim_reset_upload(ctx.room_uuid, ctx.connection_id)
        .await
    {
        Ok(true) => {
            info!(
                "Connection {} (user {}) volunteered to checkpoint room {}",
                ctx.connection_id, ctx.user_login_name, ctx.room_uuid
            );
            messages::send_reset_request(
                ctx.room_uuid,
                Some(ctx.connection_id),
                messages::ResetPhase::Upload,
                ctx.state,
            )
            .await;
        }
        Ok(false) => debug!(
            "Connection {} offered to checkpoint room {}, but the job was taken",
            ctx.connection_id, ctx.room_uuid
        ),
        Err(e) => error!(
            "Failed to claim the checkpoint for room {}: {}",
            ctx.room_uuid, e
        ),
    }
}

async fn process_server_message(
    msg_type: u8,
    data: &[u8],
    msg: &Message,
    ctx: &SessionContext<'_>,
) -> Option<Message> {
    match msg_type {
        0x01 => {
            // For JOIN messages with Redis Pub/Sub architecture:
            // 1. Process the join (sends JOIN_RESPONSE via Redis to all participants)
            // 2. Return the original message to be broadcast (but not stored - JOIN messages are ephemeral)
            // 3. Current participants are communicated via JOIN_RESPONSE, not history replay

            messages::handle_join_message(
                data,
                ctx.user_id,
                ctx.user_login_name,
                ctx.room_uuid,
                ctx.db,
                ctx.state,
            )
            .await;

            // Return the JOIN message to be stored and broadcast via Redis
            Some(msg.clone())
        }
        0x02 => {
            // Snapshot (undo/redo sync): no server-side processing needed,
            // it is sequenced and broadcast like any other history message
            Some(msg.clone())
        }
        0x03 => messages::handle_chat_message(data, ctx.user_id, ctx.user_login_name),
        0x04 => {
            // An answer to a checkpoint query. It is between this connection
            // and the server; nobody else in the room needs to hear it.
            handle_reset_offer(ctx).await;
            None
        }
        0x07 => {
            messages::handle_end_session_message(
                data,
                messages::EndSessionContext {
                    user_id: ctx.user_id,
                    user_login_name: ctx.user_login_name,
                    room_uuid: ctx.room_uuid,
                    is_owner: ctx.is_owner,
                    db: ctx.db,
                    state: ctx.state,
                    msg,
                    connection_id: ctx.connection_id,
                },
            )
            .await;

            // Message is already broadcast internally, don't re-broadcast
            None
        }
        _ => {
            debug!(
                "Unknown server message type: 0x{:02x} in room {}",
                msg_type, ctx.room_uuid
            );
            Some(msg.clone())
        }
    }
}

async fn cleanup_connection(
    connection_id: &str,
    user_login_name: &str,
    user_id: Uuid,
    room_uuid: Uuid,
    db: &sqlx::Pool<sqlx::Postgres>,
    state: &AppState,
) {
    info!(
        "Websocket connection {} (user {}) leaving room {}",
        connection_id, user_login_name, room_uuid
    );

    messages::send_leave_message(room_uuid, connection_id, user_id, user_login_name, state).await;

    if let Err(e) = db::mark_participant_inactive(db, room_uuid, user_id).await {
        error!("Failed to update participant on disconnect: {}", e);
    }

    // Unregister connection from Redis
    if let Err(e) = state.redis_state.unregister_connection(connection_id).await {
        error!(
            "Failed to unregister connection {} from Redis: {}",
            connection_id, e
        );
    }

    // On a redeploy the room is emptying because the server is going away, not
    // because everyone left: these same people are already reconnecting. Leave
    // the room's Redis state exactly as it is for them to come back to.
    if state.shutdown.is_signalled() {
        debug!(
            "Shutting down - leaving room {} state intact for reconnecting clients",
            room_uuid
        );
        return;
    }

    // Check if user has any other connections in this room
    let room_connections = state
        .redis_state
        .get_room_connections(room_uuid)
        .await
        .unwrap_or_default();
    let _user_has_other_connections = {
        let mut has_other = false;
        for conn_id in &room_connections {
            if conn_id != connection_id {
                if let Ok(Some(conn_info)) = state.redis_state.get_connection_info(conn_id).await {
                    if conn_info.user_id == user_id {
                        has_other = true;
                        break;
                    }
                }
            }
        }
        has_other
    };

    // Note: Previously removed user from Redis room presence, but now using database
    // for canonical participant ordering. User remains in collaborative_sessions_participants
    // until they explicitly leave or session ends.

    let room_connection_count = room_connections.len();

    // Only consider removing room if no connections remain
    if room_connection_count == 0 {
        // Double-check with database to see if there are still active participants
        // This prevents removing the room if participants are reconnecting
        match db::get_active_user_count(db, room_uuid).await {
            Ok(active_count) => {
                if active_count == 0 {
                    // Safe to remove room since no active participants in database
                    info!(
                        "Removing room {} - no active participants remaining",
                        room_uuid
                    );
                    // Room cleanup is now handled entirely by Redis state

                    // NOTE: We do NOT clean up Redis message history here!
                    // Redis history should persist even when no one is connected,
                    // so users can rejoin and see the previous drawing history.
                    // Redis cleanup only happens when:
                    // 1. Session is explicitly ended (END_SESSION)
                    // 2. Session is inactive for extended period (cleanup task)
                    // 3. Messages expire via TTL

                    // Clean up room presence and activity
                    if let Err(e) = state.redis_state.cleanup_room_state(room_uuid).await {
                        error!("Failed to cleanup room state for room {}: {}", room_uuid, e);
                    }
                } else {
                    debug!(
                        "Keeping room {} - {} active participants remain in database",
                        room_uuid, active_count
                    );
                }
            }
            Err(e) => {
                error!(
                    "Failed to check active participants for room cleanup: {}",
                    e
                );
                // On database error, err on the side of caution and keep the room
                debug!(
                    "Keeping room {} due to database error during cleanup check",
                    room_uuid
                );
            }
        }
    } else {
        debug!(
            "Room {} has {} connections remaining",
            room_uuid, room_connection_count
        );
    }
}
