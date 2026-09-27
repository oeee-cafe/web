//! A collaborative room's socket: joining it, catching up, and relaying what
//! each connection draws to the rest. One task per connection, from
//! `handle_socket` to `cleanup_connection`; the steps in between are the
//! submodules.

use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::state::AppState;
use axum::body::Bytes;
use axum::extract::{
    ws::close_code, ws::CloseFrame, ws::Message, ws::WebSocket, Path, Query, State,
    WebSocketUpgrade,
};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::borrow::Cow;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::{db, messages};

mod connection;
mod fanout;
mod history;
mod incoming;
mod reset;
use connection::{heartbeat_loop, setup_connection, user_has_other_connection, JoinFailure};
use fanout::should_forward_to_connection;
pub(super) use fanout::wrap_sequenced;
use history::{send_history_to_new_connection, send_recent_chat_to_new_connection};
use incoming::handle_incoming_messages;
#[cfg(test)]
pub(super) use reset::{reset_snapshot_count, valid_reset_payloads};

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

/// How much history one REPLAY_BATCH holds before compression. Bounded so
/// the client can start applying before the whole history has arrived.
const REPLAY_BATCH_BYTES: usize = 256 * 1024;

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
        handle_socket(
            socket,
            room_uuid,
            state,
            user.id,
            user.login_name.to_string(),
            resume.position(),
        )
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
        Err(failure) => {
            // Say why with a close frame rather than just dropping the socket.
            // The code is what the client reads: a policy close means the
            // session is over, full or out of ids, and no retry will change
            // that; anything else it comes back from on its own backoff.
            let (code, reason) = match failure {
                JoinFailure::Refused => (close_code::POLICY, "cannot join session"),
                JoinFailure::Unavailable => (close_code::AGAIN, "session unavailable"),
            };
            let _ = sender
                .send(Message::Close(Some(CloseFrame {
                    code,
                    reason: reason.into(),
                })))
                .await;
            return;
        }
    };

    // From here on this user is active in Postgres and this connection is in
    // the Redis registry, so every way out of the handler has to go through
    // the cleanup -- a join that fails halfway would otherwise hold a seat
    // until the session ends.
    let leave = || {
        cleanup_connection(
            &connection_id,
            &user_login_name,
            user_id,
            room_uuid,
            db,
            &state,
        )
    };

    // Tell the client its 1-byte session user id before any history arrives;
    // all its drawing messages will carry this id instead of a UUID
    let welcome = Message::Binary(messages::welcome_frame(session_user_id).into());
    if sender.send(welcome).await.is_err() {
        error!(
            "Failed to send welcome to connection {} in room {}",
            connection_id, room_uuid
        );
        leave().await;
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
        if sender.send(Message::Binary(layers.into())).await.is_err() {
            error!(
                "Failed to send the participant list to connection {} in room {}",
                connection_id, room_uuid
            );
            leave().await;
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
            leave().await;
            return;
        }
    };
    let room_subscription = room_listener.subscription();

    let (redis_tx, mut redis_rx) =
        mpsc::channel::<std::sync::Arc<super::room_fanout::Delivery>>(OUTGOING_QUEUE_LIMIT);
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
                    if !should_forward_to_connection(&room_msg.broadcast, &connection_id_clone) {
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
                // The room's Redis subscription on this process is gone (see
                // `RoomFanout`). Staying open would leave this client drawing
                // into a room it can no longer hear, so it is sent away to
                // reconnect, which subscribes afresh and resumes from its
                // last acknowledged position.
                Err(broadcast::error::RecvError::Closed) => {
                    warn!(
                        "Room stream ended for connection {} - closing so it can resume",
                        connection_id_clone
                    );
                    send_goodbye(&overflow_close_tx, close_code::AGAIN, "room stream lost");
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
    let (history_identity, max_history_seq) = match send_history_to_new_connection(
        &state,
        room_uuid,
        &mut sender,
        &connection_id,
        resume_position,
    )
    .await
    {
        Some(position) => position,
        None => {
            // Nothing was replayed and nothing said the replay was over, so
            // the client would sit in catch-up for good: it has no timer,
            // because only CAUGHT_UP can tell an empty history from a slow
            // one. Send it away to come back on its own backoff instead.
            send_goodbye(&close_tx, close_code::AGAIN, "history unavailable");
            (Uuid::nil(), 0)
        }
    };
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
                                reason: goodbye.reason.as_ref().into(),
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
                            reason: "server restarting".into(),
                        })))
                        .await;
                    break;
                }
                next = redis_rx.recv() => {
                    let Some(room_msg) = next else {
                        break;
                    };
                    let broadcast = &room_msg.broadcast;
                    let msg = match (broadcast.history_id.zip(broadcast.seq), &room_msg.sequenced) {
                        // Skip sequenced messages already delivered via history replay
                        (Some((history_id, s)), _) if history_id == history_identity && s <= max_history_seq => continue,
                        // Built once by the fanout for the whole room; this is
                        // a reference count, not a copy.
                        (Some(_), Some(frame)) => Message::Binary(frame.clone()),
                        // The ephemeral path, which is a pointer position at most.
                        _ => Message::Binary(broadcast.payload.clone().into()),
                    };
                    if sender.send(msg).await.is_err() {
                        debug!("WebSocket send failed");
                        break;
                    }
                }
                _ = keepalive.tick() => {
                    if sender.send(Message::Ping(Bytes::new())).await.is_err() {
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

    leave().await;

    redis_task.abort();
    // Gives up this connection's share of the room's subscription. The task
    // above owned the receiver, so aborting it is what makes this the last
    // reference; when it is also the room's last, the Redis subscription goes
    // with it.
    state.room_fanout.release(room_subscription).await;

    // A goodbye is only a goodbye if it reaches the wire. When one has been
    // handed over, give the outgoing task a moment to send it before the abort
    // takes the socket out from under it -- a client told "come back" resumes,
    // where a client whose socket merely died has to work out what happened.
    let goodbye_pending = close_tx
        .lock()
        .expect("close channel is never held across a panic")
        .is_none();
    if goodbye_pending {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut outgoing_task).await;
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

    // Unregistered first, so that what is left in the registry below is
    // everybody except this connection.
    if let Err(e) = state.redis_state.unregister_connection(connection_id).await {
        error!(
            "Failed to unregister connection {} from Redis: {}",
            connection_id, e
        );
    }

    // On a redeploy the room is emptying because the server is going away, not
    // because everyone left: these same people are already reconnecting, to
    // the other colour. Leave the room's Redis state exactly as it is for them
    // to come back to, and say nothing to the room -- a LEAVE now would show
    // everyone leaving at once, and marking them inactive could land after the
    // new process has marked them active again.
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
    let user_has_other_connections =
        user_has_other_connection(state, &room_connections, user_id, connection_id).await;

    // Leaving is something a user does, not a tab. With another connection
    // still open they are still here: the LEAVE carries only their user id,
    // and a client answers it by announcing in chat that they left and hiding
    // the cursor it keeps per session user id -- the cursor the remaining tab
    // is still drawing with. So neither the LEAVE nor the inactive mark is
    // for this connection to send; the user's last connection sends both.
    if user_has_other_connections {
        debug!(
            "User {} still has another connection in room {}",
            user_login_name, room_uuid
        );
    } else {
        messages::send_leave_message(room_uuid, connection_id, user_id, user_login_name, state)
            .await;

        if let Err(e) = db::mark_participant_inactive(db, room_uuid, user_id).await {
            error!("Failed to update participant on disconnect: {}", e);
        }
    }

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

#[cfg(test)]
mod forwarding_tests {
    use super::history::{replay_batch, replay_batches};
    use super::{should_forward_to_connection, REPLAY_BATCH_BYTES};
    use crate::web::handlers::collaborate::messages::MessageType;
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

    fn inflate_batch(frame: &[u8]) -> (Uuid, u32, Vec<(u64, Vec<u8>)>) {
        use flate2::read::ZlibDecoder;
        use std::io::Read;
        assert_eq!(frame[0], MessageType::ReplayBatch as u8);
        let history_id = Uuid::from_slice(&frame[1..17]).unwrap();
        let count = u32::from_le_bytes(frame[17..21].try_into().unwrap());
        let mut body = Vec::new();
        ZlibDecoder::new(&frame[21..])
            .read_to_end(&mut body)
            .unwrap();
        let mut entries = Vec::new();
        let mut at = 0;
        while at < body.len() {
            let seq = u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
            let len = u32::from_le_bytes(body[at + 8..at + 12].try_into().unwrap()) as usize;
            entries.push((seq, body[at + 12..at + 12 + len].to_vec()));
            at += 12 + len;
        }
        (history_id, count, entries)
    }

    #[test]
    fn a_replay_batch_carries_every_message_with_its_sequence() {
        let history_id = Uuid::new_v4();
        let stroke = vec![0x16u8; 40];
        let frame = replay_batch(history_id, &[(7, &[0x14, 1][..]), (8, &stroke[..])]);
        let (id, count, entries) = inflate_batch(&frame);
        assert_eq!(id, history_id);
        assert_eq!(count, 2);
        assert_eq!(entries, vec![(7, vec![0x14, 1]), (8, stroke)]);
    }

    #[test]
    fn a_replay_is_cut_into_batches_of_about_the_bound_and_never_loses_order() {
        let history_id = Uuid::new_v4();
        let messages: Vec<(u64, Vec<u8>)> =
            (1..=10).map(|seq| (seq, vec![seq as u8; 100])).collect();
        let batches = replay_batches(
            history_id,
            messages.iter().map(|(seq, bytes)| (*seq, &bytes[..])),
            250,
        );
        // 100-byte messages, 250 to a batch: two per batch, ten across five.
        assert_eq!(batches.len(), 5);
        let replayed: Vec<(u64, Vec<u8>)> = batches
            .iter()
            .flat_map(|batch| inflate_batch(batch).2)
            .collect();
        assert_eq!(replayed, messages);
    }

    #[test]
    fn a_message_larger_than_the_bound_is_a_batch_of_its_own() {
        let history_id = Uuid::new_v4();
        let snapshot = vec![0x02u8; 1000];
        let batches = replay_batches(
            history_id,
            [(1, &[0x14, 1][..]), (2, &snapshot[..]), (3, &[0x14, 1][..])],
            100,
        );
        assert_eq!(batches.len(), 3);
        assert_eq!(inflate_batch(&batches[1]).2, vec![(2, snapshot)]);
    }

    #[test]
    fn a_batched_replay_is_smaller_than_the_frames_it_replaces() {
        let history_id = Uuid::new_v4();
        // Stroke chunks as a client sends them: a 12-byte header, 32 points
        // of int16 pairs walking across the canvas, a 4-byte mask.
        let chunks: Vec<Vec<u8>> = (0..200u16)
            .map(|chunk| {
                let mut bytes = vec![0x16, 3, 3, 0, 4, 0, 0, 0, 0, 255, 32, 0];
                for point in 0..32u16 {
                    let x = chunk * 3 + point;
                    let y = 200 + (point % 5);
                    bytes.extend_from_slice(&x.to_le_bytes());
                    bytes.extend_from_slice(&y.to_le_bytes());
                }
                bytes.extend_from_slice(&[0, 0, 0, 0]);
                bytes
            })
            .collect();
        let entries: Vec<(u64, &[u8])> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| (i as u64 + 1, &c[..]))
            .collect();
        let framed: usize = entries.iter().map(|(_, c)| 25 + c.len()).sum();
        let batched: usize =
            replay_batches(history_id, entries.iter().copied(), REPLAY_BATCH_BYTES)
                .iter()
                .map(Vec::len)
                .sum();
        assert!(
            batched * 2 < framed,
            "batched {batched} bytes against {framed} framed"
        );
    }
}
