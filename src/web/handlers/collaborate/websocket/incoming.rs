//! What a client sends: checked, handled if it is the server's to handle,
//! and sequenced into history if it belongs there.

use axum::extract::{ws::close_code, ws::Message, ws::WebSocket};
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::web::handlers::collaborate::{messages, protocol, redis_messages};

use super::reset::{
    finish_reset, force_reset_request, handle_reset_offer, maybe_request_reset, parse_reset_begin,
    PendingReset,
};
use super::{send_goodbye, Goodbye, SessionContext};

pub(super) async fn handle_incoming_messages(
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    ctx: SessionContext<'_>,
    close_tx: &std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Goodbye>>>>,
    removed: &tokio::sync::Notify,
) {
    let mut pending_reset: Option<PendingReset> = None;
    let mut chat_allowance = ChatAllowance::new(std::time::Instant::now());

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
            // The owner removed this connection; nothing more it sends counts.
            _ = removed.notified() => break,
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

        let Message::Binary(data) = msg else {
            continue;
        };
        let mut data = Vec::from(data);

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

        // Somebody watching says who they are and talks, and that is all: a
        // mark, a cursor, a checkpoint or its offer from them is dropped
        // before anything reads it.
        if ctx.spectating && !matches!(data[0], 0x01 | 0x03) {
            debug!(
                "Dropping 0x{:02x} from spectator connection {} in room {}",
                data[0], ctx.connection_id, ctx.room_uuid
            );
            continue;
        }

        // Talking faster than anybody reads is flooding: every line goes to
        // the whole room, its recent chat and its recording. Past the
        // allowance a line is dropped, and the sender sees it never echo.
        if data[0] == messages::MessageType::Chat as u8
            && !chat_allowance.take(std::time::Instant::now())
        {
            debug!(
                "Dropping chat over the allowance from connection {} in room {}",
                ctx.connection_id, ctx.room_uuid
            );
            continue;
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

        // Session reset upload: RESET_BEGIN announces the snapshots, then the
        // snapshots are captured here -- they replace history instead of being
        // sequenced or broadcast (live clients already have this state; only
        // late joiners replay the reset). Before the frame is built, so the
        // bytes are moved into the upload rather than copied a second time:
        // a checkpoint is up to 64 MiB of them.
        if let Some(reset) = pending_reset.as_mut()
            && data.first() == Some(&(messages::MessageType::Snapshot as u8))
        {
            if reset.take_snapshot(data) {
                // Heavier than any checkpoint a room this size can
                // make. The room is freed to ask somebody else.
                warn!(
                    "Discarding a checkpoint over {} bytes from connection {} in room {}",
                    redis_messages::MAX_CHECKPOINT_BYTES,
                    ctx.connection_id,
                    ctx.room_uuid
                );
                let _ = ctx
                    .state
                    .redis_state
                    .clear_reset_pending(ctx.room_uuid)
                    .await;
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
        if data.first() == Some(&(messages::MessageType::ResetBegin as u8)) {
            pending_reset = parse_reset_begin(&data, &ctx).await;
            continue;
        }

        // Validation above guarantees a type byte. A server message becomes
        // whatever its handler builds from it; a client message is forwarded
        // as it is.
        let msg_type = data[0];
        let msg = if !messages::is_client_message(msg_type) {
            match process_server_message(msg_type, &data, &ctx).await {
                Some(processed_msg) => processed_msg,
                None => continue,
            }
        } else {
            Message::Binary(data.into())
        };

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
                    // The activity stamp and the history's size came back
                    // with the sequence number, inside the same script. They
                    // used to be three more round trips, taken on every
                    // drawing message before this loop would read the next one
                    // from the same client.
                    maybe_request_reset(&ctx, size).await;
                    // The message is already in the archive buffer; this is
                    // only the nudge that moves a batch of them into storage,
                    // and it is spawned because this task is in the middle of
                    // somebody's stroke.
                    crate::web::handlers::collaborate::archive::maybe_flush(
                        ctx.state,
                        ctx.room_uuid,
                        seq,
                    );
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
            // Ephemeral messages (chat, join, pointers) bypass the sequencer
            if let Message::Binary(data) = &msg
                && data.first() == Some(&(messages::MessageType::Chat as u8))
            {
                let store = redis_messages::RedisMessageStore::new(ctx.state.redis_pool.clone());
                if let Err(e) = store.append_chat_message(ctx.room_uuid, data).await {
                    error!(
                        "Failed to preserve recent chat in room {}: {}",
                        ctx.room_uuid, e
                    );
                }
                // And into the recording. `data` here is the frame the
                // server built in `process_server_message`, so the name on
                // it is the one it authenticated.
                crate::web::handlers::collaborate::archive::record_chat(
                    ctx.state,
                    ctx.room_uuid,
                    data,
                )
                .await;
            }
            messages::broadcast_message(&msg, ctx.room_uuid, ctx.connection_id, ctx.state).await;
        }
    }

    // If this connection was mid-reset, release the flag so another client
    // can be asked without waiting for the TTL. Only if the checkpoint was
    // actually ours: a connection whose unasked-for upload we were discarding
    // would otherwise take the job away from whoever really has it.
    if pending_reset.is_some_and(|reset| reset.accepted)
        && let Err(e) = ctx
            .state
            .redis_state
            .clear_reset_pending(ctx.room_uuid)
            .await
    {
        error!(
            "Failed to clear reset-pending flag for room {}: {}",
            ctx.room_uuid, e
        );
    }
}

async fn process_server_message(
    msg_type: u8,
    data: &[u8],
    ctx: &SessionContext<'_>,
) -> Option<Message> {
    match msg_type {
        0x01 => {
            // The frame the room hears is the one the handler builds, carrying
            // the name and flags this connection authenticated with. The
            // client's own 25 bytes are the bare join -- uuid and timestamp,
            // no name -- and every client's decoder refuses a JOIN shorter
            // than 28, so re-broadcasting those meant nobody was ever told who
            // joined. JOIN is ephemeral: broadcast, never stored.
            messages::handle_join_message(
                data,
                ctx.who,
                ctx.flags,
                ctx.room_uuid,
                ctx.db,
                ctx.state,
            )
            .await
        }
        0x02 => {
            // A snapshot is a layer of a checkpoint, and a checkpoint arrives
            // as a reset upload: RESET_BEGIN first, then its snapshots, which
            // are captured above before anything reaches here. One on its own
            // was not asked for, and sequenced it would overwrite a
            // participant's layers for everyone who replays the history.
            warn!(
                "Dropping a snapshot from connection {} in room {} outside a reset upload",
                ctx.connection_id, ctx.room_uuid
            );
            None
        }
        0x03 => messages::handle_chat_message(data, ctx.who, ctx.flags),
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
                    connection_id: ctx.connection_id,
                },
            )
            .await;

            // Message is already broadcast internally, don't re-broadcast
            None
        }
        // Everything else up to 0x10 is the server's to send. `validate`
        // already refuses the types it has no layout for, so this is the
        // backstop: forwarded, a WELCOME or a SESSION_EXPIRED from a client
        // would be believed by everyone in the room.
        _ => {
            debug!(
                "Dropping server message type 0x{:02x} sent by a client in room {}",
                msg_type, ctx.room_uuid
            );
            None
        }
    }
}

/// How many lines a connection may say at once before it has to wait.
/// Enough for somebody typing a few quick replies in a row.
const CHAT_BURST: f64 = 5.0;

/// How fast the allowance comes back: a line a second, which is faster than
/// anybody types and reads a conversation, and slow enough that a script
/// cannot bury the room's chat, its recent history or its recording.
const CHAT_LINES_PER_SECOND: f64 = 1.0;

/// A connection's chat allowance: a bucket of [`CHAT_BURST`] lines refilling
/// at [`CHAT_LINES_PER_SECOND`]. Kept with the connection rather than in
/// Redis, since it is asked on every line and is nobody else's business. A
/// person with two tabs has two; a socket is cheap to open, but every one
/// the owner can see and remove.
struct ChatAllowance {
    lines: f64,
    at: std::time::Instant,
}

impl ChatAllowance {
    fn new(now: std::time::Instant) -> Self {
        Self {
            lines: CHAT_BURST,
            at: now,
        }
    }

    /// Spends a line if there is one.
    fn take(&mut self, now: std::time::Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.lines = (self.lines + elapsed * CHAT_LINES_PER_SECOND).min(CHAT_BURST);
        self.at = now;
        if self.lines >= 1.0 {
            self.lines -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod chat_allowance_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_burst_then_a_line_a_second() {
        let start = Instant::now();
        let mut allowance = ChatAllowance::new(start);
        for line in 0..5 {
            assert!(allowance.take(start), "line {line} of the burst");
        }
        assert!(!allowance.take(start), "the sixth at once");
        assert!(!allowance.take(start + Duration::from_millis(500)));
        assert!(allowance.take(start + Duration::from_millis(1000)));
        assert!(!allowance.take(start + Duration::from_millis(1000)));
    }

    #[test]
    fn quiet_time_refills_only_up_to_the_burst() {
        let start = Instant::now();
        let mut allowance = ChatAllowance::new(start);
        let later = start + Duration::from_secs(3600);
        for line in 0..5 {
            assert!(allowance.take(later), "line {line}");
        }
        assert!(!allowance.take(later));
    }
}
