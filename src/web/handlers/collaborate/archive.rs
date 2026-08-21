//! An append-only recording of a room's canonical stream.
//!
//! The live history in `redis_messages` is a working set, not a record. A
//! checkpoint squashes everything at or below its base into snapshots and the
//! room's TTL takes the rest, which is exactly what keeps a busy room cheap --
//! and it is why a session that lost its drawing could only be examined
//! through whatever happened to survive in Redis an hour later.
//!
//! So this keeps the other copy: every message that was ever given a sequence
//! number, in the order it was given one, with the connection that sent it and
//! the moment it arrived. Nothing removes from it.
//!
//! Two things will read it. A replay of a finished collaboration is this log
//! applied from the beginning; a post-mortem of one that went wrong is the
//! same log read rather than rendered. Neither is built here -- this is the
//! recording, and it is the part that has to exist before either is possible,
//! because a log that was not kept cannot be added afterwards.
//!
//! ## What is and is not in it
//!
//! Everything that passes through `sequence_and_publish`, which is every
//! message that enters canonical history and the RESET_POINT that marks a
//! checkpoint. Not the checkpoint snapshots themselves: those are written
//! straight into history by the reset script, they run to megabytes, and a log
//! that starts at sequence 1 can render the same pixels from the operations.
//! `first_seq` in the manifest is what says whether a given archive does start
//! at 1 -- a session already under way when archiving was deployed does not,
//! and a reader has to be able to tell.

use aws_sdk_s3::primitives::ByteStream;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use redis::AsyncCommands;
use serde::Serialize;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::redis::RedisPool;
use crate::web::state::AppState;
use crate::AppConfig;

use super::redis_state::RoomBroadcast;

const ARCHIVE_PREFIX: &str = "oeee:archive:v1:";
const ARCHIVE_CLAIM_PREFIX: &str = "oeee:archive_claim:v1:";
const ARCHIVE_CHAT_PREFIX: &str = "oeee:archive_chat:v1:";

/// How long un-flushed entries wait in Redis before they are given up on.
///
/// Generous on purpose: this is the window in which a flush has to succeed,
/// and the cost of it being long is memory for a room nobody is in. A day
/// covers a Redis that was unreachable for an afternoon.
pub const ARCHIVE_BUFFER_TTL: u64 = 24 * 60 * 60;

/// Entries moved into one object. A chunk is written whole or not at all, so
/// this also bounds what one failed flush has to do again.
const FLUSH_BATCH: isize = 2048;

/// A sequence divisible by this asks the room to flush.
///
/// Only the connection that sent that message sees it, so the trigger fires
/// once per this many messages rather than once per participant. It is a
/// nudge, not a guarantee: the guarantees are the seal when a session ends and
/// the sweeper that finds rooms nobody ended.
pub const FLUSH_EVERY: u64 = 512;

const FLUSH_CLAIM_TTL: u64 = 120;

/// `OEEELOG` and a format byte. A chunk that does not begin with this is not
/// one of ours, and a reader should say so rather than interpret it.
const CHUNK_MAGIC: [u8; 8] = *b"OEEELOG\x01";

/// Where a session's objects live, under the bucket the images already use.
const ARCHIVE_R2_PREFIX: &str = "collaborate-archive";

/// Where recordings go, or None when this deployment has not been given
/// anywhere private to put them.
///
/// Deliberately not defaulted to the image bucket: that one is served straight
/// to browsers from `r2_public_endpoint_url`, so a recording written there
/// would be readable by anyone holding the session's id. Recording nothing is
/// the right failure.
pub fn bucket(config: &AppConfig) -> Option<&str> {
    selected_bucket(config.archive_s3_bucket.as_deref())
}

/// A blank setting means the same as an absent one: a config written out with
/// the key empty is a deployment that has not chosen a bucket, not one that
/// has chosen the empty bucket.
fn selected_bucket(configured: Option<&str>) -> Option<&str> {
    configured.filter(|name| !name.is_empty())
}

/// Named by the sequencer, which appends to it in the same step it assigns a
/// position.
pub fn buffer_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_PREFIX, room_uuid)
}

fn chat_buffer_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CHAT_PREFIX, room_uuid)
}

fn archive_claim_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CLAIM_PREFIX, room_uuid)
}

/// One recorded message: when it was sequenced, and the broadcast itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedMessage {
    /// Milliseconds since the epoch, taken on the server when the sequence was
    /// assigned. The drawing messages carry no time of their own -- a replay
    /// has no pacing without this, and a post-mortem no way to say how long a
    /// client took over anything.
    pub at: u64,
    pub broadcast: RoomBroadcast,
}

/// A chunk: the magic, then each entry as a length and its bytes.
///
/// Self-describing per entry rather than per file, because the reader that
/// matters most is the one opening a chunk from a session that went wrong,
/// possibly a truncated one. A short tail costs the entries in it and nothing
/// before them.
pub fn encode_chunk(entries: &[ArchivedMessage]) -> Vec<u8> {
    let mut out = Vec::from(CHUNK_MAGIC);
    for entry in entries {
        let frame = entry.broadcast.encode();
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.at.to_le_bytes());
        out.extend_from_slice(&frame);
    }
    out
}

/// Every whole entry in a chunk. `None` only when the magic is wrong, so a
/// file that is not an archive is distinguishable from one that is empty.
///
/// Chunks concatenate: a header met at an entry boundary is skipped rather
/// than read as a length, so a whole session downloaded as one stream reads
/// exactly like one of the objects it is made of. Everything that consumes an
/// archive then has one case instead of two.
pub fn decode_chunk(bytes: &[u8]) -> Option<Vec<ArchivedMessage>> {
    if bytes.len() < CHUNK_MAGIC.len() || bytes[..CHUNK_MAGIC.len()] != CHUNK_MAGIC {
        return None;
    }
    let mut entries = Vec::new();
    let mut at = CHUNK_MAGIC.len();
    while at + 12 <= bytes.len() {
        if bytes[at..].starts_with(&CHUNK_MAGIC) {
            at += CHUNK_MAGIC.len();
            continue;
        }
        let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let timestamp = u64::from_le_bytes([
            bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7],
            bytes[at + 8], bytes[at + 9], bytes[at + 10], bytes[at + 11],
        ]);
        let start = at + 12;
        let Some(frame) = bytes.get(start..start + len) else {
            // A truncated tail: everything whole before it still reads.
            break;
        };
        match RoomBroadcast::decode(frame) {
            Some(broadcast) => entries.push(ArchivedMessage { at: timestamp, broadcast }),
            // Framing we do not recognise. Stop rather than guess where the
            // next entry begins.
            None => break,
        }
        at = start + len;
    }
    Some(entries)
}

/// Splits a buffered `"<millis>:<frame>"`.
fn decode_buffered(entry: &[u8]) -> Option<ArchivedMessage> {
    let split = entry.iter().position(|&byte| byte == b':')?;
    let at = std::str::from_utf8(&entry[..split]).ok()?.parse().ok()?;
    Some(ArchivedMessage {
        at,
        broadcast: RoomBroadcast::decode(&entry[split + 1..])?,
    })
}

/// The un-flushed tail of a room's recording.
#[derive(Clone)]
pub struct ArchiveBuffer {
    pool: RedisPool,
}

type BufferResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

impl ArchiveBuffer {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    pub async fn pending(&self, room_uuid: Uuid) -> BufferResult<usize> {
        let mut conn = self.pool.get().await?;
        Ok(conn.llen(buffer_key(room_uuid)).await?)
    }

    /// The oldest entries, left where they are.
    ///
    /// Read before write and trimmed only once the object is stored, so a
    /// flush that dies between the two costs a repeated chunk rather than a
    /// lost one -- and a repeated chunk is written to the key its first
    /// sequence names, over identical bytes.
    pub async fn peek(&self, room_uuid: Uuid, limit: isize) -> BufferResult<Vec<ArchivedMessage>> {
        let mut conn = self.pool.get().await?;
        let raw: Vec<Vec<u8>> = conn.lrange(buffer_key(room_uuid), 0, limit - 1).await?;
        Ok(raw.iter().filter_map(|entry| decode_buffered(entry)).collect())
    }

    pub async fn drop_front(&self, room_uuid: Uuid, count: usize) -> BufferResult<()> {
        let mut conn = self.pool.get().await?;
        conn.ltrim::<_, ()>(buffer_key(room_uuid), count as isize, -1)
            .await?;
        Ok(())
    }

    /// One flusher per room at a time, so two connections that both hit the
    /// trigger do not write the same chunk twice over each other.
    pub async fn claim(&self, room_uuid: Uuid) -> BufferResult<bool> {
        let mut conn = self.pool.get().await?;
        Ok(redis::cmd("SET")
            .arg(archive_claim_key(room_uuid))
            .arg("1")
            .arg("NX")
            .arg("EX")
            .arg(FLUSH_CLAIM_TTL)
            .query_async::<Option<String>>(&mut *conn)
            .await?
            .is_some())
    }

    pub async fn release(&self, room_uuid: Uuid) -> BufferResult<()> {
        let mut conn = self.pool.get().await?;
        conn.del::<_, ()>(archive_claim_key(room_uuid)).await?;
        Ok(())
    }
}

/// One line of a room's conversation, as it is kept.
///
/// Beside the log rather than in it. Chat never reaches the sequencer -- it is
/// broadcast and forgotten, with the last hundred lines held in Redis for
/// somebody joining -- so it has no canonical position, and giving it a
/// made-up one to fit the binary format would put a thing that is not a mark
/// into the stream a canvas is rebuilt from. It is text; it is kept as text.
#[derive(Debug, Serialize)]
pub struct ArchivedChat {
    /// Milliseconds since the epoch, from the sender's own clock -- this is
    /// what the chat frame carries, and the transcript is read against the
    /// recording's timeline rather than used to order anything.
    pub at: u64,
    pub user_id: Uuid,
    pub login_name: String,
    pub message: String,
}

/// Keeps one line, if this deployment keeps anything.
///
/// Called with the frame the *server* built, which carries the name it
/// authenticated rather than the one the client claimed.
pub async fn record_chat(state: &AppState, room_uuid: Uuid, frame: &[u8]) {
    if bucket(&state.config).is_none() {
        return;
    }
    let Some(chat) = super::messages::ChatMessage::parse(frame) else {
        warn!("Could not read a chat frame for room {} to record it", room_uuid);
        return;
    };
    let line = ArchivedChat {
        at: chat.timestamp,
        user_id: chat.user_id,
        login_name: chat.username,
        message: chat.message,
    };
    let encoded = match serde_json::to_string(&line) {
        Ok(encoded) => encoded,
        Err(e) => {
            warn!("Could not encode a chat line for room {}: {}", room_uuid, e);
            return;
        }
    };
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        let mut conn = state.redis_pool.get().await?;
        let key = chat_buffer_key(room_uuid);
        conn.rpush::<_, _, ()>(&key, encoded).await?;
        conn.expire::<_, ()>(&key, ARCHIVE_BUFFER_TTL as i64).await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        warn!("Failed to record a chat line for room {}: {}", room_uuid, e);
    }
}

/// Every line held for a room, oldest first.
///
/// Never trimmed as it is written out: the transcript is stored whole each
/// time, so the buffer has to keep holding the whole thing. It is text, and a
/// session's worth of it is smaller than one snapshot.
async fn buffered_chat(
    state: &AppState,
    room_uuid: Uuid,
) -> BufferResult<Vec<serde_json::Value>> {
    let mut conn = state.redis_pool.get().await?;
    let raw: Vec<String> = conn.lrange(chat_buffer_key(room_uuid), 0, -1).await?;
    Ok(raw
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// What a reader needs that the operations do not carry.
///
/// Written beside the chunks and rewritten as they land, so an archive of a
/// session that ended badly still describes itself. The participant map is the
/// part that cannot be recovered later: the canonical stream addresses people
/// by a one-byte session id, and what that id meant lives in a Redis key with
/// an hour on it.
#[derive(Debug, Serialize)]
pub struct ArchiveManifest {
    pub format: &'static str,
    pub version: u32,
    pub session: Uuid,
    pub width: i32,
    pub height: i32,
    /// The first sequence this archive holds. Anything above 1 means the
    /// recording began mid-session and cannot be rendered from nothing.
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    /// Session id to login name, for the layer stack and for attribution.
    pub participants: Vec<ArchiveParticipant>,
    /// True once the session ended and everything buffered was written.
    pub sealed: bool,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct ArchiveParticipant {
    pub session_id: u8,
    pub user_id: Uuid,
    pub login_name: String,
}

/// The bucket client, built the same way the image upload builds it.
pub fn s3_client(config: &AppConfig) -> aws_sdk_s3::Client {
    let credentials = aws_sdk_s3::config::Credentials::new(
        config.aws_access_key_id.clone(),
        config.aws_secret_access_key.clone(),
        None,
        None,
        "",
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .endpoint_url(config.r2_endpoint_url.clone())
        .region(aws_sdk_s3::config::Region::new(config.aws_region.clone()))
        .credentials_provider(aws_sdk_s3::config::SharedCredentialsProvider::new(
            credentials,
        ))
        .behavior_version_latest()
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

/// What a stored chunk is called.
///
/// Named by the first sequence it holds, zero-padded so a plain listing is in
/// sequence order and a repeated flush overwrites itself rather than
/// duplicating. The suffix says how it is stored: recordings are mostly one
/// connection id and one history id repeated per entry, which is sixty per
/// cent of the bytes and compresses five- or sixfold.
const CHUNK_SUFFIX: &str = ".oeeelog.gz";

/// What the first chunks were written as, before they were compressed. Read
/// but never written: two of them exist and there is no reason they should
/// stop working.
const CHUNK_SUFFIX_PLAIN: &str = ".oeeelog";

fn chunk_key(room_uuid: Uuid, first_seq: u64) -> String {
    format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/{first_seq:012}{CHUNK_SUFFIX}")
}

/// Compresses a chunk for storage.
fn compress(chunk: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(chunk)?;
    encoder.finish()
}

/// Restores one, or passes it through when it was stored before compression.
///
/// Decompressed here rather than handed on compressed: what a reader gets is
/// the format its decoder is written against, in both languages, and the
/// verified reader stays untouched by how the bytes happened to be kept.
fn decompress(stored: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    if stored.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        GzDecoder::new(stored).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(stored.to_vec())
    }
}

fn manifest_key(room_uuid: Uuid) -> String {
    format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/manifest.json")
}

fn chat_key(room_uuid: Uuid) -> String {
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
            warn!("Failed to claim an archive flush for room {}: {}", room_uuid, e);
            return 0;
        }
    }

    let written = write_chunks(state, room_uuid, &buffer).await;
    let chatted = match write_chat(state, room_uuid).await {
        Ok(lines) => lines,
        Err(e) => {
            warn!("Failed to store the transcript for room {}: {}", room_uuid, e);
            0
        }
    };
    if written > 0 || chatted > 0 {
        if let Err(e) = write_manifest(state, room_uuid, false).await {
            warn!("Failed to write the archive manifest for room {}: {}", room_uuid, e);
        }
    }
    if let Err(e) = buffer.release(room_uuid).await {
        warn!("Failed to release the archive claim for room {}: {}", room_uuid, e);
    }
    written
}

async fn write_chunks(state: &AppState, room_uuid: Uuid, buffer: &ArchiveBuffer) -> usize {
    let Some(bucket) = bucket(&state.config) else {
        return 0;
    };
    let client = s3_client(&state.config);
    let mut written = 0usize;
    loop {
        let entries = match buffer.peek(room_uuid, FLUSH_BATCH).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read the archive buffer for room {}: {}", room_uuid, e);
                return written;
            }
        };
        if entries.is_empty() {
            return written;
        }
        // Ephemeral messages never reach the sequencer, so every entry here
        // has a position; the first one names the object.
        let Some(first_seq) = entries.iter().find_map(|entry| entry.broadcast.seq) else {
            error!(
                "Archive buffer for room {} holds {} entries with no sequence; dropping them",
                room_uuid,
                entries.len()
            );
            let _ = buffer.drop_front(room_uuid, entries.len()).await;
            return written;
        };
        let count = entries.len();
        let body = match compress(&encode_chunk(&entries)) {
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
            warn!("Failed to store an archive chunk for room {}: {}", room_uuid, e);
            return written;
        }

        if let Err(e) = buffer.drop_front(room_uuid, count).await {
            // The chunk is stored; failing to trim means it is written again
            // next time, over the same bytes.
            warn!("Failed to trim the archive buffer for room {}: {}", room_uuid, e);
            return written + count;
        }
        written += count;
        debug!(
            "Archived {} messages for room {} from sequence {}",
            count, room_uuid, first_seq
        );
        if (count as isize) < FLUSH_BATCH {
            return written;
        }
    }
}

/// Everything the room has, and the note that says nothing more is coming.
///
/// Called where a session ends, before the room's Redis state is cleaned up --
/// the participant map the manifest needs is one of the keys that goes.
///
/// `force` is for the one place that knows a session has just ended, which is
/// worth a manifest even if the last flush already emptied the buffer. The
/// sweeper does not know that: it re-examines every ended session on every
/// pass, so sealing unconditionally there wrote a manifest per session per
/// five minutes -- thousands of objects an hour, for sessions that had nothing
/// recorded at all. A room with nothing buffered has nothing left to seal.
pub async fn seal_room(state: &AppState, room_uuid: Uuid, force: bool) {
    if bucket(&state.config).is_none() {
        return;
    }
    if !force {
        let drawing = ArchiveBuffer::new(state.redis_pool.clone())
            .pending(room_uuid)
            .await;
        let chat = buffered_chat(state, room_uuid).await.map(|held| held.len());
        match (drawing, chat) {
            // A room that talked and never drew still has something to keep.
            (Ok(0), Ok(0)) => return,
            (Ok(_), _) | (_, Ok(_)) => {}
            (Err(e), _) => {
                warn!("Failed to read the archive buffer for room {}: {}", room_uuid, e);
                return;
            }
        }
    }
    let flushed = flush_room(state, room_uuid).await;
    if let Err(e) = write_manifest(state, room_uuid, true).await {
        warn!("Failed to seal the archive for room {}: {}", room_uuid, e);
        return;
    }
    info!(
        "Sealed the archive for room {} ({} messages in this pass)",
        room_uuid, flushed
    );
}

async fn write_manifest(
    state: &AppState,
    room_uuid: Uuid,
    sealed: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let session = sqlx::query!(
        "SELECT width, height FROM collaborative_sessions WHERE id = $1",
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

    let (first_seq, last_seq) = archived_bounds(state, room_uuid).await;

    let manifest = ArchiveManifest {
        format: "oeee-collab-archive",
        version: 1,
        session: room_uuid,
        width: session.width,
        height: session.height,
        first_seq,
        last_seq,
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
    Ok(())
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

/// The transcript as stored, for the viewer and for staff reading one back.
pub async fn read_chat(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let object = s3_client(&state.config)
        .get_object()
        .bucket(bucket)
        .key(chat_key(room_uuid))
        .send()
        .await;
    match object {
        Ok(object) => {
            let bytes = object.body.collect().await?.into_bytes();
            Ok(serde_json::from_slice(&bytes)?)
        }
        // A room where nobody said anything has no transcript, which is an
        // answer rather than a failure.
        Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(404) => Ok(Vec::new()),
        Err(e) => Err(Box::new(e)),
    }
}

/// The span the stored chunks cover, from their names and the buffer's tail.
///
/// Read from the listing rather than remembered, so a manifest rewritten by a
/// later flush describes what is actually in the bucket.
async fn archived_bounds(state: &AppState, room_uuid: Uuid) -> (Option<u64>, Option<u64>) {
    let Some(bucket) = bucket(&state.config) else {
        return (None, None);
    };
    let client = s3_client(&state.config);
    let listed = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
        .send()
        .await;
    let mut first = None;
    match listed {
        Ok(listed) => {
            for object in listed.contents() {
                let Some(key) = object.key() else { continue };
                let Some(name) = key.rsplit('/').next() else { continue };
                let Some(stem) = name
                    .strip_suffix(CHUNK_SUFFIX)
                    .or_else(|| name.strip_suffix(CHUNK_SUFFIX_PLAIN))
                else {
                    continue;
                };
                if let Ok(seq) = stem.parse::<u64>() {
                    first = Some(first.map_or(seq, |held: u64| held.min(seq)));
                }
            }
        }
        Err(e) => warn!("Failed to list the archive for room {}: {}", room_uuid, e),
    }

    // The newest sequence the room has reached, which the last stored chunk
    // ends on once the buffer is empty.
    let store = super::redis_messages::RedisMessageStore::new(state.redis_pool.clone());
    let last = store.current_sequence(room_uuid).await.ok().flatten();
    (first, last)
}

/// The largest report accepted. The trace is bounded at 512 events by the
/// painter, so this is headroom rather than a limit anyone should meet.
pub const MAX_DIAGNOSTIC_BYTES: usize = 512 * 1024;

/// Files one client's account of what it believed, beside the session's log.
///
/// Under the archive's own prefix on purpose: a report is only ever read
/// together with the stream it disagrees with, and keeping them in two places
/// is how one of them gets cleaned up without the other.
pub async fn store_diagnostic(
    state: &AppState,
    room_uuid: Uuid,
    user_login_name: &str,
    body: Vec<u8>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(None);
    };
    let key = format!(
        "{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/{}-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        user_login_name,
    );
    s3_client(&state.config)
        .put_object()
        .bucket(bucket)
        .key(&key)
        .content_type("application/json")
        .body(ByteStream::from(body))
        .send()
        .await?;
    Ok(Some(key))
}

/// Everything stored for a session, its objects end to end, and the manifest
/// beside them.
///
/// Whole rather than paged: the point of holding one of these is to run it
/// through the client and watch where the canvas goes wrong, and a session is
/// a few hundred kilobytes. Anything buffered but not yet flushed is written
/// out first, so what comes back is current rather than up to five hundred
/// messages behind.
pub async fn download_session(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    flush_room(state, room_uuid).await;

    let client = s3_client(&state.config);
    let listed = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
        .send()
        .await?;

    let mut keys: Vec<String> = listed
        .contents()
        .iter()
        .filter_map(|object| object.key())
        .filter(|key| key.ends_with(CHUNK_SUFFIX) || key.ends_with(CHUNK_SUFFIX_PLAIN))
        .map(|key| key.to_string())
        .collect();
    // Names are the zero-padded first sequence, so this is sequence order.
    keys.sort();

    let mut out = Vec::new();
    for key in keys {
        let object = client
            .get_object()
            .bucket(bucket)
            .key(&key)
            .send()
            .await?;
        out.extend_from_slice(&decompress(&object.body.collect().await?.into_bytes())?);
    }
    Ok(out)
}

/// What a recording says about itself: canvas, participants, and the span it
/// covers. None when the session has no recording.
pub async fn read_manifest(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(None);
    };
    let object = s3_client(&state.config)
        .get_object()
        .bucket(bucket)
        .key(manifest_key(room_uuid))
        .send()
        .await;
    match object {
        Ok(object) => {
            let bytes = object.body.collect().await?.into_bytes();
            Ok(Some(serde_json::from_slice(&bytes)?))
        }
        // A session that was never recorded has no manifest, which is an
        // answer rather than a failure.
        Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(404) => Ok(None),
        Err(e) => Err(Box::new(e)),
    }
}

/// Every synchronisation report filed for a session, newest last.
///
/// Returned as one JSON array so a session's reports can be read together:
/// what matters is usually the difference between what two clients believed at
/// the same moment, which is a comparison and not a file.
pub async fn download_diagnostics(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let client = s3_client(&state.config);
    let listed = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/"))
        .send()
        .await?;

    // The key begins with the moment it was filed, so this is chronological.
    let mut keys: Vec<String> = listed
        .contents()
        .iter()
        .filter_map(|object| object.key())
        .map(|key| key.to_string())
        .collect();
    keys.sort();

    let mut reports = Vec::new();
    for key in keys {
        let object = client
            .get_object()
            .bucket(bucket)
            .key(&key)
            .send()
            .await?;
        let bytes = object.body.collect().await?.into_bytes();
        match serde_json::from_slice(&bytes) {
            Ok(report) => reports.push(report),
            Err(e) => warn!("Skipping unreadable diagnostic {}: {}", key, e),
        }
    }
    Ok(reports)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message(at: u64, seq: u64, payload: &[u8]) -> ArchivedMessage {
        ArchivedMessage {
            at,
            broadcast: RoomBroadcast {
                from_connection: "conn-1".to_string(),
                target_connection: None,
                seq: Some(seq),
                history_id: Some(Uuid::from_u128(7)),
                payload: payload.to_vec(),
            },
        }
    }

    #[test]
    fn a_chunk_round_trips_every_entry_in_order() {
        let entries = vec![
            message(1_700_000_000_000, 1, &[0x16, 0x01, 0x02]),
            message(1_700_000_000_050, 2, &[0x14, 0x01]),
            message(1_700_000_000_900, 3, &[]),
        ];
        let decoded = decode_chunk(&encode_chunk(&entries)).expect("a chunk");
        assert_eq!(decoded, entries);
    }

    /// Drawing payloads are arbitrary bytes, and the length prefix is what
    /// keeps a delimiter out of the question.
    #[test]
    fn a_payload_that_looks_like_framing_survives() {
        let entries = vec![message(5, 9, b"OEEELOG\x01\x00\x00\x00\x0a:|\n1|")];
        let decoded = decode_chunk(&encode_chunk(&entries)).expect("a chunk");
        assert_eq!(decoded, entries);
    }

    /// A session is downloaded as its objects end to end, so the reader has to
    /// treat that as one log rather than as the first chunk followed by
    /// rubbish.
    #[test]
    fn chunks_concatenate_into_one_readable_log() {
        let first = vec![message(1, 1, &[0x16, 0x01]), message(2, 2, &[0x16, 0x02])];
        let second = vec![message(3, 3, &[0x14, 0x01])];
        let mut joined = encode_chunk(&first);
        joined.extend_from_slice(&encode_chunk(&second));

        let decoded = decode_chunk(&joined).expect("a log");
        assert_eq!(decoded, [first, second].concat());
    }

    #[test]
    fn an_empty_chunk_is_still_a_chunk() {
        assert_eq!(decode_chunk(&encode_chunk(&[])), Some(Vec::new()));
    }

    #[test]
    fn rejects_bytes_that_are_not_an_archive() {
        assert_eq!(decode_chunk(b""), None);
        assert_eq!(decode_chunk(b"OEEELOG\x02"), None);
        assert_eq!(decode_chunk(b"not a log at all"), None);
    }

    /// A chunk cut off mid-write -- a flush that died, a truncated download --
    /// gives up everything whole before the cut and nothing else. The point of
    /// a forensic log is that a bad tail does not cost the head.
    #[test]
    fn a_truncated_chunk_reads_up_to_the_cut() {
        let entries = vec![
            message(1, 1, &[0x16; 8]),
            message(2, 2, &[0x17; 8]),
            message(3, 3, &[0x18; 8]),
        ];
        let whole = encode_chunk(&entries);
        // From the magic onwards: a file cut before that is not identifiable
        // as an archive at all, which `rejects_bytes_that_are_not_an_archive`
        // is the case for.
        for cut in CHUNK_MAGIC.len()..whole.len() {
            let decoded = decode_chunk(&whole[..cut]).expect("a chunk");
            assert!(
                decoded.len() <= entries.len(),
                "cut {cut} produced more entries than were written"
            );
            assert_eq!(decoded[..], entries[..decoded.len()], "cut {cut}");
        }
    }

    #[test]
    fn a_buffered_entry_round_trips_through_its_redis_encoding() {
        let entry = message(1_700_000_000_123, 42, &[0x16, 0x03]);
        let mut raw = format!("{}:", entry.at).into_bytes();
        raw.extend_from_slice(&entry.broadcast.encode());
        assert_eq!(decode_buffered(&raw), Some(entry));
    }

    #[test]
    fn rejects_a_buffered_entry_without_a_timestamp() {
        assert_eq!(decode_buffered(b"1|conn||1|\npayload"), None);
        assert_eq!(decode_buffered(b""), None);
    }

    /// The trigger fires once per window, on whichever connection sent that
    /// message -- not once per participant per window.
    #[test]
    fn only_one_sequence_in_each_window_asks_for_a_flush() {
        let asked: Vec<u64> = (1..=(FLUSH_EVERY * 2))
            .filter(|seq| seq.is_multiple_of(FLUSH_EVERY))
            .collect();
        assert_eq!(asked, vec![FLUSH_EVERY, FLUSH_EVERY * 2]);
    }

    /// Recording is off unless somewhere private has been named for it.
    ///
    /// The image bucket is served straight to browsers, so defaulting to it
    /// would publish every session's traffic to anyone holding the id. An
    /// empty setting is the same as an absent one, because a config written
    /// out with the key blank means the same thing as one without it.
    #[test]
    fn recording_is_off_until_a_bucket_is_named_for_it() {
        assert_eq!(selected_bucket(None), None);
        assert_eq!(selected_bucket(Some("")), None);
        assert_eq!(
            selected_bucket(Some("oeee-cafe-archive")),
            Some("oeee-cafe-archive")
        );
    }

    /// Storage is compressed; what a reader is handed is not. The decoders on
    /// both sides are written against the plain format and there is no reason
    /// they should have to know how the bytes were kept.
    #[test]
    fn a_chunk_survives_being_compressed_for_storage() {
        let entries = vec![
            message(1_700_000_000_000, 1, &[0x16, 0x01, 0x02]),
            message(1_700_000_000_050, 2, &[0x14, 0x01]),
        ];
        let plain = encode_chunk(&entries);
        let stored = compress(&plain).expect("compress");
        assert_eq!(decompress(&stored).expect("decompress"), plain);
        assert_eq!(decode_chunk(&decompress(&stored).unwrap()), Some(entries));
    }

    /// Two chunks were written before this, and there is no reason they should
    /// stop reading.
    #[test]
    fn a_chunk_stored_before_compression_still_reads() {
        let entries = vec![message(5, 9, &[0x16, 0x03])];
        let plain = encode_chunk(&entries);
        assert_eq!(decompress(&plain).expect("passthrough"), plain);
    }

    /// The saving is the whole reason for it. Real recordings run five- or
    /// sixfold; this only asks that a body of repeated framing does not come
    /// out bigger than it went in.
    #[test]
    fn compressing_a_recording_makes_it_smaller() {
        let entries: Vec<ArchivedMessage> = (1..200)
            .map(|seq| message(1_700_000_000_000 + seq, seq, &[0x16, 0x01, 0x02, 0x03]))
            .collect();
        let plain = encode_chunk(&entries);
        let stored = compress(&plain).expect("compress");
        assert!(
            stored.len() * 3 < plain.len(),
            "{} compressed to {}",
            plain.len(),
            stored.len()
        );
    }

    #[test]
    fn chunk_keys_sort_in_sequence_order() {
        let room = Uuid::from_u128(1);
        let mut keys = vec![
            chunk_key(room, 1024),
            chunk_key(room, 1),
            chunk_key(room, 99),
        ];
        keys.sort();
        assert_eq!(
            keys,
            vec![chunk_key(room, 1), chunk_key(room, 99), chunk_key(room, 1024)]
        );
    }
}
