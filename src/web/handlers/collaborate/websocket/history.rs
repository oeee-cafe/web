//! Catching a new connection up: the replay in batches, then recent chat.

use crate::web::state::AppState;
use axum::extract::{ws::Message, ws::WebSocket};
use futures_util::stream::SplitSink;
use futures_util::SinkExt;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::web::handlers::collaborate::{messages, redis_messages};

use super::REPLAY_BATCH_BYTES;

// Returns the history's identity and the highest sequence number the replay
// reached (0 for an empty history), or None when the history could not be
// read at all -- in which case the client was told nothing and must not be
// left waiting.
/// One REPLAY_BATCH frame: the history id, how many messages, then the
/// messages as `[seq:8][len:4][bytes]` runs under zlib.
///
/// Every join and every resume is replayed this way. A replay is up to the
/// auto-reset threshold of messages, and framed one by one each cost a
/// 25-byte envelope and a turn of the client's processing chain; the stroke
/// points inside them are int16 pairs that repeat their high bytes, and
/// deflate to less than half.
///
/// `[0x10][history_id:16][count:4][zlib(entries)]`, all little-endian. Each
/// entry is what the same message's SEQUENCED frame would carry after its
/// envelope, so a client applies one exactly as it applies the other.
pub(super) fn replay_batch(history_id: Uuid, entries: &[(u64, &[u8])]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let body_len: usize = entries.iter().map(|(_, bytes)| 12 + bytes.len()).sum();
    let mut body = Vec::with_capacity(body_len);
    for (seq, bytes) in entries {
        body.extend_from_slice(&seq.to_le_bytes());
        body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(bytes);
    }
    let mut frame = Vec::with_capacity(21 + body_len / 2);
    frame.push(messages::MessageType::ReplayBatch as u8);
    frame.extend_from_slice(history_id.as_bytes());
    frame.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    // Fast: a join is waiting on this, and the win over sending the bytes as
    // they are comes from the first pass, not from the last percent.
    let mut encoder = ZlibEncoder::new(frame, Compression::fast());
    encoder
        .write_all(&body)
        .expect("writing to a Vec cannot fail");
    encoder
        .finish()
        .expect("finishing a Vec-backed encoder cannot fail")
}

/// Splits a replay into batches of about `max_bytes` of history each.
///
/// A message larger than the bound is a batch of its own: a checkpoint's
/// snapshots are each a PNG of the whole canvas.
pub(super) fn replay_batches<'a>(
    history_id: Uuid,
    entries: impl IntoIterator<Item = (u64, &'a [u8])>,
    max_bytes: usize,
) -> Vec<Vec<u8>> {
    let mut batches = Vec::new();
    let mut pending: Vec<(u64, &[u8])> = Vec::new();
    let mut pending_bytes = 0;
    for entry in entries {
        if !pending.is_empty() && pending_bytes + entry.1.len() > max_bytes {
            batches.push(replay_batch(history_id, &pending));
            pending.clear();
            pending_bytes = 0;
        }
        pending_bytes += entry.1.len();
        pending.push(entry);
    }
    if !pending.is_empty() {
        batches.push(replay_batch(history_id, &pending));
    }
    batches
}

pub(super) async fn send_history_to_new_connection(
    state: &AppState,
    room_uuid: Uuid,
    sender: &mut SplitSink<WebSocket, Message>,
    connection_id: &str,
    resume_position: Option<(Uuid, u64)>,
) -> Option<(Uuid, u64)> {
    let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());

    match redis_store
        .get_history_since(room_uuid, resume_position)
        .await
    {
        Ok(since) => {
            let redis_messages::HistorySince {
                history_id,
                max_seq: current_max_seq,
                after_seq,
                entries,
            } = since;
            // Sent, or already on the client's canvas: what the live stream
            // may skip. Rises with what is fed below.
            let mut max_seq = after_seq;
            match resume_position {
                Some((resume_history_id, resume_seq))
                    if after_seq == resume_seq && resume_history_id == history_id =>
                {
                    debug!(
                        "Resuming connection {} in history {} after seq {}",
                        connection_id, history_id, resume_seq
                    );
                }
                Some(_) => {
                    debug!(
                        "Resume position rejected for {}; sending full history",
                        connection_id
                    );
                }
                None => {}
            }
            let replay_start = messages::replay_start_frame(history_id, after_seq, current_max_seq);
            if sender
                .send(Message::Binary(replay_start.into()))
                .await
                .is_err()
            {
                warn!("Failed to send replay boundary to {}", connection_id);
                return Some((history_id, after_seq));
            }
            let to_send: Vec<(u64, &[u8])> = entries
                .iter()
                .filter_map(|(seq, stored)| match stored {
                    Message::Binary(data) => Some((*seq, &data[..])),
                    _ => None,
                })
                .collect();
            // `feed` rather than `send`: `send` flushes each frame on its
            // own, and the flush below covers all of them.
            let mut fed = true;
            for batch in replay_batches(history_id, to_send.iter().copied(), REPLAY_BATCH_BYTES) {
                if sender.feed(Message::Binary(batch.into())).await.is_err() {
                    fed = false;
                    break;
                }
            }
            if fed {
                max_seq = max_seq.max(to_send.last().map(|(seq, _)| *seq).unwrap_or(0));
            } else {
                warn!(
                    "Failed to send stored history to new connection {}",
                    connection_id
                );
            }
            if sender.flush().await.is_err() {
                warn!("Failed to flush replayed history to {}", connection_id);
                return Some((history_id, max_seq));
            }
            debug!(
                "Sent {} stored messages from Redis to new connection {} (max seq {})",
                to_send.len(),
                connection_id,
                max_seq
            );
            let caught_up = messages::caught_up_frame(history_id, max_seq);
            if sender
                .send(Message::Binary(caught_up.into()))
                .await
                .is_err()
            {
                warn!("Failed to send caught-up marker to {}", connection_id);
            }
            Some((history_id, max_seq))
        }
        Err(e) => {
            error!(
                "Failed to retrieve message history from Redis for connection {}: {}",
                connection_id, e
            );
            None
        }
    }
}

pub(super) async fn send_recent_chat_to_new_connection(
    state: &AppState,
    room_uuid: Uuid,
    sender: &mut SplitSink<WebSocket, Message>,
    connection_id: &str,
) {
    let store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
    match store.get_recent_chat(room_uuid).await {
        Ok(messages) => {
            for payload in &messages {
                if sender
                    .send(Message::Binary(payload.clone().into()))
                    .await
                    .is_err()
                {
                    warn!("Failed to replay recent chat to {}", connection_id);
                    break;
                }
            }
            debug!(
                "Sent {} recent chat messages to {}",
                messages.len(),
                connection_id
            );
        }
        Err(e) => error!("Failed to load recent chat for room {}: {}", room_uuid, e),
    }
}
