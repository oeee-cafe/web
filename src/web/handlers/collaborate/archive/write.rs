//! Writing the recording out: chunks as the room goes, and the manifest and
//! chat when it is sealed.

use aws_sdk_s3::primitives::ByteStream;
use redis::AsyncCommands;
use serde::Serialize;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::web::state::AppState;

use super::{
    bounds_key, bucket, buffered_chat, clear_chat_buffer, compress_off_thread, describe,
    encode_chunk, s3_client, ArchiveBuffer, ArchivedMessage, ARCHIVE_BUFFER_TTL, ARCHIVE_R2_PREFIX,
    FLUSH_BATCH, FLUSH_EVERY, SEAL_CLAIM_ATTEMPTS,
};

/// What a reader needs that the operations do not carry.
///
/// Written beside the chunks and rewritten as they land, so an archive of a
/// session that ended badly still describes itself. The participant map is the
/// part that cannot be recovered later: the canonical stream addresses people
/// by a one-byte session id, and what that id meant lives in a Redis key with
/// an hour on it.
///
/// The account id is kept alongside the login name because a login name can be
/// changed and the account it belongs to cannot -- a replay from last year
/// should still credit the right person.
#[derive(Debug, Serialize)]
pub struct ArchiveManifest {
    pub format: &'static str,
    pub version: u32,
    pub session: Uuid,
    pub canvas: ArchiveCanvas,
    /// When the room was made, and when it was ended if it was.
    pub started_at: String,
    pub ended_at: Option<String>,
    /// How long the session was open. None while it is still going.
    pub duration_ms: Option<i64>,
    pub recording: ArchiveSpan,
    /// Session id to account and login name, in join order -- which is the
    /// order ids are handed out, and so the order layers stack in.
    pub participants: Vec<ArchiveParticipant>,
    /// True once the session ended and everything buffered was written.
    pub sealed: bool,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ArchiveCanvas {
    pub width: i32,
    pub height: i32,
    /// Which painter this room ran. Only `standard` exists today -- the
    /// collaborative page mounts nothing else -- and it is recorded anyway,
    /// because the day a second one is offered every archive written before it
    /// would otherwise be ambiguous about which it was, with no way left to
    /// find out.
    pub mode: &'static str,
}

/// What the stored chunks actually hold, as opposed to what the room reached.
#[derive(Debug, Serialize)]
pub struct ArchiveSpan {
    /// The first sequence archived. Anything but 1 means the recording began
    /// mid-session and cannot be rendered from nothing.
    pub first_seq: Option<u64>,
    /// The last sequence archived -- what is in the file, not where the room
    /// got to. A reader checking completeness needs the former.
    pub last_seq: Option<u64>,
    /// The span the messages cover, for a player that wants to draw a scrub
    /// bar before it has downloaded the log.
    pub first_at: Option<u64>,
    pub last_at: Option<u64>,
    pub messages: u64,
}

#[derive(Debug, Serialize)]
pub struct ArchiveParticipant {
    pub session_id: u8,
    pub user_id: Uuid,
    pub login_name: String,
}

/// What a stored chunk is called.
///
/// Named by the first sequence it holds, zero-padded so a plain listing is in
/// sequence order and a repeated flush overwrites itself rather than
/// duplicating. The suffix says how it is stored: recordings are mostly one
/// connection id and one history id repeated per entry, which is sixty per
/// cent of the bytes and compresses five- or sixfold.
pub(super) const CHUNK_SUFFIX: &str = ".oeeelog.gz";

/// What the first chunks were written as, before they were compressed. Read
/// but never written: two of them exist and there is no reason they should
/// stop working.
pub(super) const CHUNK_SUFFIX_PLAIN: &str = ".oeeelog";

pub(super) fn chunk_key(room_uuid: Uuid, first_seq: u64) -> String {
    format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/{first_seq:012}{CHUNK_SUFFIX}")
}

pub(super) fn manifest_key(room_uuid: Uuid) -> String {
    format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/manifest.json")
}

pub(super) fn chat_key(room_uuid: Uuid) -> String {
    format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/chat.json")
}

/// Moves everything buffered for a room into the bucket.
///
/// Returns how many messages were written. Never an error the caller has to
/// handle: a recording is not worth failing a drawing over, and what does not
/// flush now stays buffered for the next attempt.
pub async fn flush_room(state: &AppState, room_uuid: Uuid) -> usize {
    if bucket(&state.config).is_none() {
        return 0;
    }
    let buffer = ArchiveBuffer::new(state.redis_pool.clone());
    match buffer.claim(room_uuid).await {
        Ok(true) => {}
        // Somebody else is doing it.
        Ok(false) => return 0,
        Err(e) => {
            warn!(
                "Failed to claim an archive flush for room {}: {}",
                room_uuid, e
            );
            return 0;
        }
    }

    let (written, chatted) = flush_claimed(state, room_uuid, &buffer).await;
    if written > 0 || chatted > 0 {
        if let Err(e) = write_manifest(state, room_uuid, false).await {
            warn!(
                "Failed to write the archive manifest for room {}: {}",
                room_uuid,
                describe(&*e)
            );
        }
    }
    if let Err(e) = buffer.release(room_uuid).await {
        warn!(
            "Failed to release the archive claim for room {}: {}",
            room_uuid, e
        );
    }
    written
}

/// The flush itself, for a caller holding the claim: the chunks, then the
/// transcript. Returns how many messages and how many lines were written.
async fn flush_claimed(
    state: &AppState,
    room_uuid: Uuid,
    buffer: &ArchiveBuffer,
) -> (usize, usize) {
    let written = write_chunks(state, room_uuid, buffer).await;
    let chatted = match write_chat(state, room_uuid).await {
        Ok(lines) => {
            if lines > 0 {
                if let Err(e) = crate::models::collaborative_recording::note_chat_lines(
                    &state.db_pool,
                    room_uuid,
                    lines,
                )
                .await
                {
                    warn!(
                        "Failed to note the transcript length for room {}: {}",
                        room_uuid, e
                    );
                }
            }
            lines
        }
        Err(e) => {
            warn!(
                "Failed to store the transcript for room {}: {}",
                room_uuid,
                describe(&*e)
            );
            0
        }
    };
    (written, chatted)
}

async fn write_chunks(state: &AppState, room_uuid: Uuid, buffer: &ArchiveBuffer) -> usize {
    let Some(bucket) = bucket(&state.config) else {
        return 0;
    };
    let client = s3_client(&state.config);
    let mut written = 0usize;
    loop {
        let (entries, raw) = match buffer.peek_batch(room_uuid, FLUSH_BATCH).await {
            Ok(batch) => batch,
            Err(e) => {
                warn!(
                    "Failed to read the archive buffer for room {}: {}",
                    room_uuid, e
                );
                return written;
            }
        };
        if entries.is_empty() {
            if raw > 0 {
                // A run of entries nothing can read. Left there they would
                // stop every flush at this point for the rest of the session.
                warn!(
                    "Dropping {} unreadable archive entries for room {}",
                    raw, room_uuid
                );
                if buffer.drop_front(room_uuid, raw).await.is_err() {
                    return written;
                }
                continue;
            }
            return written;
        }
        if let Err(e) = buffer.extend_claim(room_uuid).await {
            warn!(
                "Failed to keep the archive claim for room {}: {}",
                room_uuid, e
            );
        }
        // Ephemeral messages never reach the sequencer, so every entry here
        // has a position; the first one names the object.
        let first_seq = entries[0].seq;
        let history_id = entries[0].history_id;
        let count = entries.len();
        let body = match compress_off_thread(encode_chunk(history_id, &entries)).await {
            Ok(body) => body,
            Err(e) => {
                error!("Failed to compress a chunk for room {}: {}", room_uuid, e);
                return written;
            }
        };

        if let Err(e) = client
            .put_object()
            .bucket(bucket)
            .key(chunk_key(room_uuid, first_seq))
            .content_type("application/gzip")
            .body(ByteStream::from(body))
            .send()
            .await
        {
            // Left in the buffer on purpose: the next flush writes the same
            // chunk to the same key.
            warn!(
                "Failed to store an archive chunk for room {}: {}",
                room_uuid,
                describe(&e)
            );
            return written;
        }

        if let Err(e) = buffer.drop_front(room_uuid, raw).await {
            // The chunk is stored; failing to trim means it is written again
            // next time, over the same bytes.
            warn!(
                "Failed to trim the archive buffer for room {}: {}",
                room_uuid, e
            );
            return written + count;
        }
        if let Err(e) = note_written(state, room_uuid, &entries).await {
            warn!(
                "Failed to record what was archived for room {}: {}",
                room_uuid, e
            );
        }
        written += count;
        debug!(
            "Archived {} messages for room {} from sequence {}",
            count, room_uuid, first_seq
        );
        if (raw as isize) < FLUSH_BATCH {
            return written;
        }
    }
}

/// Everything the room has, and the note that says nothing more is coming.
///
/// Called where a session ends, before the room's Redis state is cleaned up --
/// the participant map the manifest needs is one of the keys that goes. A
/// recording already sealed is left exactly as it is: its manifest is final,
/// and the map it was written from is gone.
///
/// `force` is for the one place that knows a session has just ended, which is
/// worth a manifest even if the last flush already emptied the buffer. The
/// sweeper does not know that: it re-examines every ended session on every
/// pass, so sealing unconditionally there wrote a manifest per session per
/// five minutes -- thousands of objects an hour, for sessions that had nothing
/// recorded at all. A room with nothing recorded and nothing buffered has
/// nothing to seal; one with a recording that is not yet sealed -- a seal
/// that failed on an earlier pass -- is sealed now.
///
/// Holds the flush claim itself across the flush and the manifest. Taken as
/// two steps, a flush already under way made `flush_room` return at once,
/// the manifest said sealed with messages still buffered, and the other
/// flusher then rewrote it unsealed from a participant map that the cleanup
/// had meanwhile deleted.
pub async fn seal_room(state: &AppState, room_uuid: Uuid, force: bool) {
    if bucket(&state.config).is_none() {
        return;
    }
    let recorded =
        match crate::models::collaborative_recording::sealed_state(&state.db_pool, room_uuid).await
        {
            Ok(Some(true)) => return,
            Ok(state) => state.is_some(),
            Err(e) => {
                warn!(
                    "Failed to read the recording state for room {}: {}",
                    room_uuid, e
                );
                false
            }
        };
    if !force && !recorded {
        let drawing = ArchiveBuffer::new(state.redis_pool.clone())
            .pending(room_uuid)
            .await;
        let chat = buffered_chat(state, room_uuid).await.map(|held| held.len());
        match (drawing, chat) {
            // A room that talked and never drew still has something to keep.
            (Ok(0), Ok(0)) => return,
            (Ok(_), _) | (_, Ok(_)) => {}
            (Err(e), _) => {
                warn!(
                    "Failed to read the archive buffer for room {}: {}",
                    room_uuid, e
                );
                return;
            }
        }
    }

    let buffer = ArchiveBuffer::new(state.redis_pool.clone());
    let mut claimed = false;
    for _ in 0..SEAL_CLAIM_ATTEMPTS {
        match buffer.claim(room_uuid).await {
            Ok(true) => {
                claimed = true;
                break;
            }
            Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            Err(e) => {
                warn!(
                    "Failed to claim the archive of room {} to seal it: {}",
                    room_uuid, e
                );
                return;
            }
        }
    }
    if !claimed {
        warn!(
            "Room {} is still being flushed; leaving it for the sweep to seal",
            room_uuid
        );
        return;
    }
    let (flushed, _) = flush_claimed(state, room_uuid, &buffer).await;
    let sealed = write_manifest(state, room_uuid, true).await;
    if let Err(e) = buffer.release(room_uuid).await {
        warn!(
            "Failed to release the archive claim for room {}: {}",
            room_uuid, e
        );
    }
    if let Err(e) = sealed {
        warn!(
            "Failed to seal the archive for room {}: {}",
            room_uuid,
            describe(&*e)
        );
        return;
    }
    // The transcript is in storage whole. Held here it only made the sweep
    // find something buffered and seal this room again on every pass for a
    // day.
    if let Err(e) = clear_chat_buffer(state, room_uuid).await {
        warn!(
            "Failed to clear the transcript buffer for room {}: {}",
            room_uuid, e
        );
    }
    info!(
        "Sealed the archive for room {} ({} messages in this pass)",
        room_uuid, flushed
    );
}

/// What a room has actually had written out.
///
/// Kept as it is written rather than worked out afterwards: the alternative is
/// downloading every chunk to find out what is in them, and the manifest is
/// rewritten on every flush.
#[derive(Debug, Default)]
struct ArchivedBounds {
    first_seq: Option<u64>,
    last_seq: Option<u64>,
    first_at: Option<u64>,
    last_at: Option<u64>,
    messages: u64,
}

async fn note_written(
    state: &AppState,
    room_uuid: Uuid,
    entries: &[ArchivedMessage],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(first) = entries.first() else {
        return Ok(());
    };
    let last = entries.last().unwrap_or(first);
    let mut conn = state.redis_pool.get().await?;
    let key = bounds_key(room_uuid);
    // The firsts are set once and never moved; the lasts follow the newest
    // chunk. A repeated flush of the same chunk therefore restates them rather
    // than double-counting anything but the message tally, which is the one
    // field a retry can inflate -- and a tally high by a chunk is a better
    // failure than a manifest that lies about the span.
    conn.hset_nx::<_, _, _, ()>(&key, "first_seq", first.seq)
        .await?;
    conn.hset_nx::<_, _, _, ()>(&key, "first_at", first.at)
        .await?;
    conn.hset::<_, _, _, ()>(&key, "last_seq", last.seq).await?;
    conn.hset::<_, _, _, ()>(&key, "last_at", last.at).await?;
    conn.hincr::<_, _, _, ()>(&key, "messages", entries.len() as i64)
        .await?;
    conn.expire::<_, ()>(&key, ARCHIVE_BUFFER_TTL as i64)
        .await?;
    Ok(())
}

async fn archived_bounds(state: &AppState, room_uuid: Uuid) -> ArchivedBounds {
    let read: Result<std::collections::HashMap<String, u64>, _> = async {
        let mut conn = state.redis_pool.get().await?;
        let held: std::collections::HashMap<String, u64> =
            conn.hgetall(bounds_key(room_uuid)).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(held)
    }
    .await;
    match read {
        Ok(held) => ArchivedBounds {
            first_seq: held.get("first_seq").copied(),
            last_seq: held.get("last_seq").copied(),
            first_at: held.get("first_at").copied(),
            last_at: held.get("last_at").copied(),
            messages: held.get("messages").copied().unwrap_or(0),
        },
        Err(e) => {
            warn!(
                "Failed to read archive bounds for room {}: {}",
                room_uuid, e
            );
            ArchivedBounds::default()
        }
    }
}

/// Stores the transcript, whole, and returns how many lines it holds.
///
/// Rewritten rather than appended because object storage has no append and a
/// session's conversation is a few kilobytes: writing all of it each time is
/// both simpler and idempotent, where stitching would have to know what it had
/// already written.
async fn write_chat(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(0);
    };
    let lines = buffered_chat(state, room_uuid).await?;
    if lines.is_empty() {
        return Ok(0);
    }
    s3_client(&state.config)
        .put_object()
        .bucket(bucket)
        .key(chat_key(room_uuid))
        .content_type("application/json")
        .body(ByteStream::from(serde_json::to_vec(&lines)?))
        .send()
        .await?;
    Ok(lines.len())
}

async fn write_manifest(
    state: &AppState,
    room_uuid: Uuid,
    sealed: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A sealed manifest is final. The participant map it was written from
    // goes with the room's Redis state, so a rewrite -- an admin download
    // flushing a stray line, a flush that raced the seal -- would replace
    // it with one that names nobody and says the recording is open.
    if crate::models::collaborative_recording::sealed_state(&state.db_pool, room_uuid).await?
        == Some(true)
    {
        return Ok(());
    }
    let session = sqlx::query!(
        "SELECT width, height, created_at, ended_at FROM collaborative_sessions WHERE id = $1",
        room_uuid
    )
    .fetch_optional(&state.db_pool)
    .await?;
    let Some(session) = session else {
        return Ok(());
    };

    let assigned = state.redis_state.get_user_ids(room_uuid).await?;
    let mut participants = Vec::new();
    if !assigned.is_empty() {
        let user_ids: Vec<Uuid> = assigned.keys().copied().collect();
        let rows = sqlx::query!(
            "SELECT id, login_name FROM users WHERE id = ANY($1)",
            &user_ids
        )
        .fetch_all(&state.db_pool)
        .await?;
        for row in rows {
            if let Some(session_id) = assigned.get(&row.id) {
                participants.push(ArchiveParticipant {
                    session_id: *session_id,
                    user_id: row.id,
                    login_name: row.login_name,
                });
            }
        }
        participants.sort_by_key(|participant| participant.session_id);
    }

    let bounds = archived_bounds(state, room_uuid).await;
    let started = session.created_at.and_utc();
    let manifest = ArchiveManifest {
        format: "oeee-collab-archive",
        version: 2,
        session: room_uuid,
        canvas: ArchiveCanvas {
            width: session.width,
            height: session.height,
            mode: "standard",
        },
        started_at: started.to_rfc3339(),
        ended_at: session.ended_at.map(|at| at.to_rfc3339()),
        duration_ms: session.ended_at.map(|at| (at - started).num_milliseconds()),
        recording: ArchiveSpan {
            first_seq: bounds.first_seq,
            last_seq: bounds.last_seq,
            first_at: bounds.first_at,
            last_at: bounds.last_at,
            messages: bounds.messages,
        },
        participants,
        sealed,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    let Some(bucket) = bucket(&state.config) else {
        return Ok(());
    };
    s3_client(&state.config)
        .put_object()
        .bucket(bucket)
        .key(manifest_key(room_uuid))
        .content_type("application/json")
        .body(ByteStream::from(serde_json::to_vec(&manifest)?))
        .send()
        .await?;
    // And where the admin list can read it without fetching this. After the
    // manifest, so the list never claims a span the manifest does not.
    if let Err(e) = crate::models::collaborative_recording::note_span(
        &state.db_pool,
        room_uuid,
        manifest.recording.first_seq,
        manifest.recording.last_seq,
        manifest.recording.messages,
        sealed,
    )
    .await
    {
        warn!(
            "Failed to note the recording span for room {}: {}",
            room_uuid, e
        );
    }
    Ok(())
}

/// Records a message that has just been sequenced, when the sequence says so.
///
/// Spawned rather than awaited: a flush is a handful of round trips to object
/// storage, and the connection that happened to send the five-hundredth
/// message is in the middle of a stroke.
pub fn maybe_flush(state: &AppState, room_uuid: Uuid, seq: u64) {
    if seq == 0 || !seq.is_multiple_of(FLUSH_EVERY) || bucket(&state.config).is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        flush_room(&state, room_uuid).await;
    });
}
