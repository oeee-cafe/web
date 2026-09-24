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
use futures_util::{StreamExt, TryStreamExt};
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
const ARCHIVE_BOUNDS_PREFIX: &str = "oeee:archive_bounds:v1:";

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
/// How long a seal waits for a flush already under way to finish, in
/// attempts of a quarter second. A flush is a few object writes; one that
/// takes longer than this is stuck, and the sweep will seal on its next pass.
const SEAL_CLAIM_ATTEMPTS: u32 = 40;
/// The most transcript lines kept for one session. The transcript is stored
/// whole on every flush, so an unbounded one would be re-uploaded in full
/// each time; a session's chat is far below this.
const MAX_CHAT_LINES: isize = 5000;
/// The most reports read back for one session. Keys sort by the moment
/// filed, so these are the newest.
const MAX_DIAGNOSTICS_READ: usize = 100;

/// How many of a session's chunks are fetched at once.
///
/// A round trip to object storage is about forty milliseconds from the app
/// host, and a session is one chunk per five hundred messages -- fetched one
/// after another, a long afternoon's drawing would spend over a second in
/// nothing but waiting. Bounded rather than unbounded because the whole
/// recording is held in memory while it is assembled, and forty requests at
/// once buys nothing over eight.
const FETCH_CONCURRENCY: usize = 8;

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

fn bounds_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_BOUNDS_PREFIX, room_uuid)
}

fn chat_buffer_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CHAT_PREFIX, room_uuid)
}

fn archive_claim_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CLAIM_PREFIX, room_uuid)
}

/// One recorded message.
///
/// Flat on purpose: what a reader wants is the position, the moment and the
/// bytes. The identifiers that are the same for every message in a chunk --
/// the room's history and the small set of connections that spoke -- are in
/// the chunk's header, written once. Repeating them per entry is what made a
/// recording sixty per cent framing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedMessage {
    /// Milliseconds since the epoch, taken on the server when the sequence was
    /// assigned. Drawing messages carry no time of their own, so this is the
    /// only thing a replay can pace itself by.
    pub at: u64,
    /// Canonical position. Contiguous in a whole recording.
    pub seq: u64,
    /// The connection that sent it, or the literal `system` for the messages
    /// the server sequences on the room's behalf.
    pub sender: String,
    /// Which history this position belongs to. Copied from the chunk header,
    /// so a reader can see a replaced history without having to track chunks.
    pub history_id: Uuid,
    /// The message itself, exactly as the room received it.
    pub payload: Vec<u8>,
}

/// The kind of an entry.
///
/// One byte that costs a recording almost nothing and is the reason this
/// format can grow. Everything today is a canonical message; a keyframe, a
/// marker, or anything else added later gets its own kind, and a reader that
/// predates it skips the entry by its length rather than losing the file.
const KIND_MESSAGE: u8 = 0;

/// `OEEELOG` and the format version.
const CHUNK_MAGIC: [u8; 8] = *b"OEEELOG\x02";

/// Fixed part of an entry: kind, sender index, sequence, time, length.
const ENTRY_HEADER: usize = 1 + 1 + 8 + 8 + 4;

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A chunk: what is true of all of it, then each message.
///
/// ```text
/// header := "OEEELOG\x02" | history_id (16) | senders (u8 count, each u8 len + utf8)
/// entry  := kind (u8) | sender index (u8) | seq (u64) | at (u64) | len (u32) | payload
/// ```
///
/// Chunks concatenate: a header met at an entry boundary starts a new one, so
/// a whole session downloaded end to end reads exactly like one of the objects
/// it is made of.
pub fn encode_chunk(history_id: Uuid, messages: &[ArchivedMessage]) -> Vec<u8> {
    let mut senders: Vec<&str> = Vec::new();
    for message in messages {
        if !senders.iter().any(|held| *held == message.sender) {
            senders.push(&message.sender);
        }
    }
    // More than this many connections in one chunk cannot be indexed by a
    // byte. A room seats eight, so it takes a chunk spanning thirty-one
    // reconnections to reach it; the rest are recorded as `system`, which is
    // wrong about who sent them and right about everything else.
    senders.truncate(255);

    let mut out = Vec::from(CHUNK_MAGIC);
    out.extend_from_slice(history_id.as_bytes());
    out.push(senders.len() as u8);
    for sender in &senders {
        let bytes = sender.as_bytes();
        let length = bytes.len().min(255);
        out.push(length as u8);
        out.extend_from_slice(&bytes[..length]);
    }

    for message in messages {
        let index = senders
            .iter()
            .position(|held| *held == message.sender)
            .unwrap_or(0);
        out.push(KIND_MESSAGE);
        out.push(index as u8);
        put_u64(&mut out, message.seq);
        put_u64(&mut out, message.at);
        out.extend_from_slice(&(message.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&message.payload);
    }
    out
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut held = [0u8; 8];
    held.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(held)
}

/// Reads a chunk header, returning what it says and where the entries start.
fn read_header(bytes: &[u8], at: usize) -> Option<(Uuid, Vec<String>, usize)> {
    let mut cursor = at + CHUNK_MAGIC.len();
    let raw: [u8; 16] = bytes.get(cursor..cursor + 16)?.try_into().ok()?;
    let history_id = Uuid::from_bytes(raw);
    cursor += 16;
    let count = *bytes.get(cursor)? as usize;
    cursor += 1;
    let mut senders = Vec::with_capacity(count);
    for _ in 0..count {
        let length = *bytes.get(cursor)? as usize;
        cursor += 1;
        let name = std::str::from_utf8(bytes.get(cursor..cursor + length)?).ok()?;
        senders.push(name.to_string());
        cursor += length;
    }
    Some((history_id, senders, cursor))
}

/// Every whole entry in a recording, in order.
///
/// `None` only when the file does not begin with a header, so "not ours" is
/// distinguishable from "empty". A truncated tail costs the entries in it and
/// nothing before them: the file most worth reading is the one from a session
/// that went wrong, which is also the one most likely to be short.
pub fn decode_chunk(bytes: &[u8]) -> Option<Vec<ArchivedMessage>> {
    if !bytes.starts_with(&CHUNK_MAGIC) {
        return None;
    }
    let (mut history_id, mut senders, mut at) = read_header(bytes, 0)?;
    let mut messages = Vec::new();
    while at + ENTRY_HEADER <= bytes.len() {
        if bytes[at..].starts_with(&CHUNK_MAGIC) {
            // The next chunk of a concatenated stream: everything below is
            // true of it instead.
            let (next_history, next_senders, next_at) = match read_header(bytes, at) {
                Some(header) => header,
                None => break,
            };
            history_id = next_history;
            senders = next_senders;
            at = next_at;
            continue;
        }
        let kind = bytes[at];
        let sender = bytes[at + 1] as usize;
        let seq = read_u64(bytes, at + 2);
        let stamp = read_u64(bytes, at + 10);
        let length = u32::from_le_bytes([
            bytes[at + 18],
            bytes[at + 19],
            bytes[at + 20],
            bytes[at + 21],
        ]) as usize;
        let start = at + ENTRY_HEADER;
        let Some(payload) = bytes.get(start..start + length) else {
            break;
        };
        at = start + length;
        // A kind this reader predates is skipped by its length rather than
        // guessed at. That is what the byte is for.
        if kind != KIND_MESSAGE {
            continue;
        }
        messages.push(ArchivedMessage {
            at: stamp,
            seq,
            sender: senders.get(sender).cloned().unwrap_or_default(),
            history_id,
            payload: payload.to_vec(),
        });
    }
    Some(messages)
}

/// Splits a buffered `"<millis>:<frame>"`.
///
/// The buffer keeps the broadcast as it went out, which is what the sequencer
/// already had in hand; flattening it is this side's job.
fn decode_buffered(entry: &[u8]) -> Option<ArchivedMessage> {
    let split = entry.iter().position(|&byte| byte == b':')?;
    let at = std::str::from_utf8(&entry[..split]).ok()?.parse().ok()?;
    let broadcast = RoomBroadcast::decode(&entry[split + 1..])?;
    Some(ArchivedMessage {
        at,
        seq: broadcast.seq?,
        sender: broadcast.from_connection,
        history_id: broadcast.history_id.unwrap_or_default(),
        payload: broadcast.payload,
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

    /// Everything waiting, for a reader that wants the recording as it
    /// stands without writing anything out.
    pub async fn peek_all(&self, room_uuid: Uuid) -> BufferResult<Vec<ArchivedMessage>> {
        let mut conn = self.pool.get().await?;
        let raw: Vec<Vec<u8>> = conn.lrange(buffer_key(room_uuid), 0, -1).await?;
        Ok(raw.iter().filter_map(|entry| decode_buffered(entry)).collect())
    }

    /// `peek`, and also how many raw entries were read: the count to trim
    /// once the chunk is stored. An entry that does not decode is still one
    /// entry in the list, and trimming by the decoded count instead left it
    /// there, put the next chunk's first message twice in storage, and a run
    /// of them stopped the flusher for good with the buffer still growing.
    pub async fn peek_batch(
        &self,
        room_uuid: Uuid,
        limit: isize,
    ) -> BufferResult<(Vec<ArchivedMessage>, usize)> {
        let mut conn = self.pool.get().await?;
        let raw: Vec<Vec<u8>> = conn.lrange(buffer_key(room_uuid), 0, limit - 1).await?;
        let entries = raw
            .iter()
            .filter_map(|entry| decode_buffered(entry))
            .collect();
        Ok((entries, raw.len()))
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

    /// Keeps the claim while a long flush runs, so it cannot lapse between
    /// two chunks and let a second flusher trim what this one is writing.
    pub async fn extend_claim(&self, room_uuid: Uuid) -> BufferResult<()> {
        let mut conn = self.pool.get().await?;
        conn.expire::<_, ()>(archive_claim_key(room_uuid), FLUSH_CLAIM_TTL as i64)
            .await?;
        Ok(())
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
        conn.ltrim::<_, ()>(&key, -MAX_CHAT_LINES, -1).await?;
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

/// An error from storage, with the reason in it.
///
/// The SDK's own Display for a failed request is the two words "service
/// error"; what R2 actually said -- AccessDenied, NoSuchBucket -- is further
/// down the source chain, and printing the error with `{}` drops it. Every
/// place that logs or returns a storage failure goes through this.
pub fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    aws_sdk_s3::error::DisplayErrorContext(e).to_string()
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

/// Compresses a recording, for storing one chunk or for sending a whole
/// session over the wire.
pub fn compress(chunk: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(chunk)?;
    encoder.finish()
}

/// `compress`, off the async runtime.
///
/// A chunk is a few hundred kilobytes and a whole session's download is
/// megabytes; either way it is CPU work, and done on a worker thread it
/// held that thread while every socket the thread was serving waited.
pub async fn compress_off_thread(chunk: Vec<u8>) -> Result<Vec<u8>, std::io::Error> {
    tokio::task::spawn_blocking(move || compress(&chunk))
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
}

/// `decompress`, off the async runtime, for the same reason.
async fn decompress_off_thread(stored: Vec<u8>) -> Result<Vec<u8>, std::io::Error> {
    tokio::task::spawn_blocking(move || decompress(&stored))
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
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
        warn!("Failed to release the archive claim for room {}: {}", room_uuid, e);
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
                    warn!("Failed to note the transcript length for room {}: {}", room_uuid, e);
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
                warn!("Failed to read the archive buffer for room {}: {}", room_uuid, e);
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
            warn!("Failed to trim the archive buffer for room {}: {}", room_uuid, e);
            return written + count;
        }
        if let Err(e) = note_written(state, room_uuid, &entries).await {
            warn!("Failed to record what was archived for room {}: {}", room_uuid, e);
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
                warn!("Failed to read the archive buffer for room {}: {}", room_uuid, e);
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
        warn!("Failed to release the archive claim for room {}: {}", room_uuid, e);
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

async fn clear_chat_buffer(state: &AppState, room_uuid: Uuid) -> BufferResult<()> {
    let mut conn = state.redis_pool.get().await?;
    conn.del::<_, ()>(chat_buffer_key(room_uuid)).await?;
    Ok(())
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
    conn.hset_nx::<_, _, _, ()>(&key, "first_seq", first.seq).await?;
    conn.hset_nx::<_, _, _, ()>(&key, "first_at", first.at).await?;
    conn.hset::<_, _, _, ()>(&key, "last_seq", last.seq).await?;
    conn.hset::<_, _, _, ()>(&key, "last_at", last.at).await?;
    conn.hincr::<_, _, _, ()>(&key, "messages", entries.len() as i64)
        .await?;
    conn.expire::<_, ()>(&key, ARCHIVE_BUFFER_TTL as i64).await?;
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
            warn!("Failed to read archive bounds for room {}: {}", room_uuid, e);
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

/// The transcript, for the viewer and for staff reading one back.
///
/// From the Redis buffer while it lasts, since that holds every line -- it is
/// never trimmed as it is written out -- including the ones said since the
/// last flush. Read there rather than flushed first: the inspector asks every
/// few seconds while it follows a live room, and a flush per ask would write a
/// sliver of a chunk per ask. Storage is the answer once the buffer expires.
pub async fn read_chat(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    match buffered_chat(state, room_uuid).await {
        Ok(held) if !held.is_empty() => return Ok(held),
        Ok(_) => {}
        Err(e) => warn!("Failed to read the buffered transcript for room {}: {}", room_uuid, e),
    }
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
        duration_ms: session
            .ended_at
            .map(|at| (at - started).num_milliseconds()),
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
        warn!("Failed to note the recording span for room {}: {}", room_uuid, e);
    }
    Ok(())
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
    // A few random characters after the moment, so two reports filed in the
    // same millisecond -- a client files two back to back when a checkpoint
    // fails and the socket then closes on the gap -- do not overwrite.
    let key = format!(
        "{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/{}-{}-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        user_login_name,
        &Uuid::new_v4().simple().to_string()[..6],
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

/// Every key under a prefix, across every page the bucket answers with.
///
/// One page is a thousand keys, in name order: taken alone, a session with
/// more chunks than that came back without its newest ones, and read as
/// complete.
async fn list_keys(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: String,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    let mut keys = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page?;
        keys.extend(
            page.contents()
                .iter()
                .filter_map(|object| object.key())
                .map(str::to_string),
        );
    }
    Ok(keys)
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
    let mut keys: Vec<String> =
        list_keys(&client, bucket, format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
            .await?
            .into_iter()
            .filter(|key| key.ends_with(CHUNK_SUFFIX) || key.ends_with(CHUNK_SUFFIX_PLAIN))
            .collect();
    // Names are the zero-padded first sequence, so this is sequence order.
    keys.sort();

    assemble(keys, |key| {
        let client = client.clone();
        let bucket = bucket.to_string();
        async move {
            let object = client.get_object().bucket(bucket).key(&key).send().await?;
            let bytes = object.body.collect().await?.into_bytes();
            Ok(decompress_off_thread(bytes.to_vec()).await?)
        }
    })
    .await
}

/// The recording after `after`, as it stands, without writing anything out.
///
/// For following a live session: the inspector asks every few seconds, and
/// `download_session` would flush on each ask and leave a chunk of a few
/// messages behind every time. This reads the stored chunks that can hold
/// anything later than `after` -- the one it falls inside and those after it,
/// found by name -- and then whatever is still buffered in Redis.
///
/// May repeat messages at or before `after`, and may repeat one between a
/// stored chunk and the buffer while a flush is under way; a reader keeps what
/// is past the last sequence it has. Empty when there is nothing new.
pub async fn read_tail(
    state: &AppState,
    room_uuid: Uuid,
    after: u64,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let client = s3_client(&state.config);
    let mut chunks: Vec<(u64, String)> =
        list_keys(&client, bucket, format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
            .await?
            .into_iter()
            .filter_map(|key| chunk_first_seq(&key).map(|first| (first, key)))
            .collect();
    chunks.sort();
    let keys = chunks_after(&chunks, after);

    let mut out = assemble(keys, |key| {
        let client = client.clone();
        let bucket = bucket.to_string();
        async move {
            let object = client.get_object().bucket(bucket).key(&key).send().await?;
            let bytes = object.body.collect().await?.into_bytes();
            Ok(decompress_off_thread(bytes.to_vec()).await?)
        }
    })
    .await?;

    let buffered = ArchiveBuffer::new(state.redis_pool.clone())
        .peek_all(room_uuid)
        .await?;
    out.extend(encode_runs(
        buffered.into_iter().filter(|message| message.seq > after).collect(),
    ));
    Ok(out)
}

/// The first sequence a chunk holds, from its name; None for anything under
/// the prefix that is not a chunk.
fn chunk_first_seq(key: &str) -> Option<u64> {
    let name = key.rsplit('/').next()?;
    let stem = name
        .strip_suffix(CHUNK_SUFFIX)
        .or_else(|| name.strip_suffix(CHUNK_SUFFIX_PLAIN))?;
    stem.parse().ok()
}

/// The chunks that can hold a sequence past `after`: the last one starting at
/// or before it, which it may fall inside, and every one starting later.
/// `chunks` is sorted by first sequence.
fn chunks_after(chunks: &[(u64, String)], after: u64) -> Vec<String> {
    let from = chunks
        .iter()
        .rposition(|(first, _)| *first <= after)
        .unwrap_or(0);
    chunks[from..].iter().map(|(_, key)| key.clone()).collect()
}

/// Buffered messages as chunks, one per run of the same history. A reset
/// replaces the history mid-buffer, and a chunk header names one.
fn encode_runs(messages: Vec<ArchivedMessage>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut start = 0;
    for end in 1..=messages.len() {
        if end == messages.len() || messages[end].history_id != messages[start].history_id {
            out.extend(encode_chunk(messages[start].history_id, &messages[start..end]));
            start = end;
        }
    }
    out
}

/// Fetches a session's chunks and joins them into one log.
///
/// Separate from where the bytes come from because the property that matters
/// here is the joining: several requests are in flight at once and they finish
/// in whatever order they like, so a bug would show up as a recording that is
/// silently out of order rather than as a failure. `buffered` yields results
/// in the order the keys were given however the requests complete, which is
/// what makes that safe -- and what the test below pins.
///
/// A chunk that fails takes the whole download with it. A recording with a
/// hole in it that reads as whole is worse than one that failed to arrive.
async fn assemble<F, Fut>(
    keys: Vec<String>,
    fetch: F,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>>,
{
    let chunks: Vec<Vec<u8>> = futures_util::stream::iter(keys)
        .map(fetch)
        .buffered(FETCH_CONCURRENCY)
        .try_collect()
        .await?;

    let mut out = Vec::with_capacity(chunks.iter().map(|chunk| chunk.len()).sum());
    for chunk in chunks {
        out.extend_from_slice(&chunk);
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
) -> Result<Vec<FiledDiagnostic>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let client = s3_client(&state.config);
    // The key begins with the moment it was filed, so this is chronological.
    let mut keys = list_keys(
        &client,
        bucket,
        format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/"),
    )
    .await?;
    keys.sort();
    // The newest, bounded: each is a fetch and up to half a megabyte held.
    if keys.len() > MAX_DIAGNOSTICS_READ {
        keys.drain(..keys.len() - MAX_DIAGNOSTICS_READ);
    }

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
            Ok(report) => {
                let (filed_at, filed_by) = filed_as(&key);
                reports.push(FiledDiagnostic {
                    filed_at,
                    filed_by,
                    key,
                    report,
                })
            }
            Err(e) => warn!("Skipping unreadable diagnostic {}: {}", key, e),
        }
    }
    Ok(reports)
}

/// One report, with what only its storage key knows.
///
/// The body is whatever the client posted, and the client does not say who
/// it is -- the server does, by writing the authenticated login name into the
/// key. A reader comparing two clients needs that name beside the report.
#[derive(Debug, Serialize)]
pub struct FiledDiagnostic {
    /// When the server received it, as written into the key.
    pub filed_at: Option<String>,
    pub filed_by: Option<String>,
    pub key: String,
    pub report: serde_json::Value,
}

/// Reads `.../diagnostics/<timestamp>-<login>.json` back into its parts.
///
/// Split at the first hyphen: the timestamp has none, and a login name may.
fn filed_as(key: &str) -> (Option<String>, Option<String>) {
    let Some(name) = key
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".json"))
    else {
        return (None, None);
    };
    // Less the random tail `store_diagnostic` adds; a name from before it
    // had none.
    let name = match name.rsplit_once('-') {
        Some((rest, tail)) if tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_hexdigit()) => rest,
        _ => name,
    };
    match name.split_once('-') {
        Some((at, by)) => {
            let at = chrono::NaiveDateTime::parse_from_str(at, "%Y%m%dT%H%M%S%.3fZ")
                .ok()
                .map(|at| at.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            (at, Some(by.to_string()))
        }
        None => (None, None),
    }
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

    const HISTORY: Uuid = Uuid::from_u128(7);

    fn message(at: u64, seq: u64, payload: &[u8]) -> ArchivedMessage {
        sent_by("conn-1", at, seq, payload)
    }

    fn sent_by(sender: &str, at: u64, seq: u64, payload: &[u8]) -> ArchivedMessage {
        ArchivedMessage {
            at,
            seq,
            sender: sender.to_string(),
            history_id: HISTORY,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn a_chunk_round_trips_every_entry_in_order() {
        let entries = vec![
            message(1_700_000_000_000, 1, &[0x16, 0x01, 0x02]),
            sent_by("conn-2", 1_700_000_000_050, 2, &[0x14, 0x01]),
            sent_by("system", 1_700_000_000_900, 3, &[]),
        ];
        let decoded = decode_chunk(&encode_chunk(HISTORY, &entries)).expect("a chunk");
        assert_eq!(decoded, entries);
    }

    /// The connection that sent each message survives, from a table written
    /// once rather than a name repeated on every entry -- and `system`, which
    /// is what the server sequences the room's own messages under, is not a
    /// UUID and never needed to be.
    #[test]
    fn senders_are_named_once_and_pointed_at() {
        let entries = vec![
            sent_by("conn-a", 1, 1, &[0x16]),
            sent_by("conn-b", 2, 2, &[0x16]),
            sent_by("conn-a", 3, 3, &[0x16]),
            sent_by("system", 4, 4, &[0x0d]),
        ];
        let encoded = encode_chunk(HISTORY, &entries);
        // Three distinct names, each written once however many messages they
        // sent.
        assert_eq!(encoded.windows(6).filter(|w| *w == b"conn-a").count(), 1);
        assert_eq!(encoded.windows(6).filter(|w| *w == b"system").count(), 1);
        assert_eq!(decode_chunk(&encoded).expect("a chunk"), entries);
    }

    /// Drawing payloads are arbitrary bytes, and the length prefix is what
    /// keeps a delimiter out of the question.
    #[test]
    fn a_payload_that_looks_like_framing_survives() {
        let entries = vec![message(5, 9, b"OEEELOG\x02\x00\x00\x00\x0a:|\n1|")];
        let decoded = decode_chunk(&encode_chunk(HISTORY, &entries)).expect("a chunk");
        assert_eq!(decoded, entries);
    }

    /// A session is downloaded as its objects end to end, so the reader has to
    /// treat that as one log -- and pick up the second chunk's own history and
    /// sender table rather than reading it through the first one's.
    #[test]
    fn chunks_concatenate_into_one_readable_log() {
        let other = Uuid::from_u128(11);
        let first = vec![sent_by("conn-a", 1, 1, &[0x16, 0x01])];
        let second = vec![ArchivedMessage {
            at: 2,
            seq: 2,
            sender: "conn-z".to_string(),
            history_id: other,
            payload: vec![0x14],
        }];
        let mut joined = encode_chunk(HISTORY, &first);
        joined.extend_from_slice(&encode_chunk(other, &second));

        let decoded = decode_chunk(&joined).expect("a log");
        assert_eq!(decoded, [first, second].concat());
        // A replaced history is visible without the reader tracking chunks.
        assert_eq!(decoded[0].history_id, HISTORY);
        assert_eq!(decoded[1].history_id, other);
    }

    #[test]
    fn an_empty_chunk_is_still_a_chunk() {
        assert_eq!(decode_chunk(&encode_chunk(HISTORY, &[])), Some(Vec::new()));
    }

    /// The byte that lets this format grow. An entry of a kind written after
    /// this reader is skipped by its length, so a file with one in it still
    /// yields everything else rather than being lost.
    #[test]
    fn an_entry_of_an_unknown_kind_is_skipped_not_fatal() {
        let known = vec![message(1, 1, &[0x16, 0x01]), message(3, 3, &[0x16, 0x03])];
        let mut encoded = encode_chunk(HISTORY, &known);
        // Splice a future entry in between: kind 99, sender 0, seq 2, one byte.
        let mut future = vec![99u8, 0];
        future.extend_from_slice(&2u64.to_le_bytes());
        future.extend_from_slice(&2u64.to_le_bytes());
        future.extend_from_slice(&1u32.to_le_bytes());
        future.push(0xff);
        let split = encoded.len() - (ENTRY_HEADER + 2);
        encoded.splice(split..split, future);

        assert_eq!(decode_chunk(&encoded).expect("a chunk"), known);
    }

    #[test]
    fn rejects_bytes_that_are_not_an_archive() {
        assert_eq!(decode_chunk(b""), None);
        assert_eq!(decode_chunk(b"OEEELOG\x01"), None);
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
        let whole = encode_chunk(HISTORY, &entries);
        // From the header onwards: a file cut before that is not identifiable
        // as an archive at all.
        let header = read_header(&whole, 0).expect("a header").2;
        for cut in header..whole.len() {
            let decoded = decode_chunk(&whole[..cut]).expect("a chunk");
            assert_eq!(decoded[..], entries[..decoded.len()], "cut {cut}");
        }
    }

    /// The buffer keeps the broadcast as it went out; flattening it into a
    /// recorded message is this side's job.
    #[test]
    fn a_buffered_entry_round_trips_through_its_redis_encoding() {
        let broadcast = RoomBroadcast {
            from_connection: "conn-1".to_string(),
            target_connection: None,
            seq: Some(42),
            history_id: Some(HISTORY),
            payload: vec![0x16, 0x03],
        };
        let mut raw = b"1700000000123:".to_vec();
        raw.extend_from_slice(&broadcast.encode());
        assert_eq!(
            decode_buffered(&raw),
            Some(message(1_700_000_000_123, 42, &[0x16, 0x03]))
        );
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
    fn a_tail_reads_the_chunk_it_falls_inside_and_every_later_one() {
        let chunks: Vec<(u64, String)> = [1, 513, 1025]
            .into_iter()
            .map(|first| (first, chunk_key(Uuid::nil(), first)))
            .collect();
        let firsts = |after| -> Vec<u64> {
            chunks_after(&chunks, after)
                .iter()
                .map(|key| chunk_first_seq(key).unwrap())
                .collect()
        };
        assert_eq!(firsts(0), vec![1, 513, 1025]);
        assert_eq!(firsts(600), vec![513, 1025]);
        // At a chunk's first sequence the chunk before it is already read.
        assert_eq!(firsts(1025), vec![1025]);
        assert_eq!(firsts(5000), vec![1025]);
        assert!(chunks_after(&[], 10).is_empty());
    }

    #[test]
    fn only_chunks_are_read_as_chunks() {
        assert_eq!(chunk_first_seq("collaborate-archive/x/000000000513.oeeelog.gz"), Some(513));
        assert_eq!(chunk_first_seq("collaborate-archive/x/000000000001.oeeelog"), Some(1));
        assert_eq!(chunk_first_seq("collaborate-archive/x/manifest.json"), None);
        assert_eq!(chunk_first_seq("collaborate-archive/x/diagnostics/a-b.json"), None);
    }

    /// A reset replaces the history partway through what is buffered, and a
    /// chunk header names one history, so the tail is as many chunks as there
    /// are runs -- and still reads back as one log.
    #[test]
    fn a_tail_across_a_reset_names_each_history() {
        let other = Uuid::from_u128(8);
        let mut messages = vec![message(10, 1, b"a"), message(11, 2, b"b")];
        let mut after_reset = message(12, 3, b"c");
        after_reset.history_id = other;
        messages.push(after_reset);
        let decoded = decode_chunk(&encode_runs(messages.clone())).unwrap();
        assert_eq!(decoded, messages);
        assert!(encode_runs(Vec::new()).is_empty());
    }

    #[test]
    fn a_report_key_says_when_it_was_filed_and_by_whom() {
        let (at, by) = filed_as(
            "collaborate-archive/00000000-0000-0000-0000-000000000001/diagnostics/20260924T084655.527Z-some-one.json",
        );
        assert_eq!(at.as_deref(), Some("2026-09-24T08:46:55.527Z"));
        // A hyphen in the login name stays in the login name.
        assert_eq!(by.as_deref(), Some("some-one"));
    }

    #[test]
    fn a_report_key_of_another_shape_is_not_guessed_at() {
        assert_eq!(filed_as("collaborate-archive/x/diagnostics/report.json"), (None, None));
    }

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
        let plain = encode_chunk(HISTORY, &entries);
        let stored = compress(&plain).expect("compress");
        assert_eq!(decompress(&stored).expect("decompress"), plain);
        assert_eq!(decode_chunk(&decompress(&stored).unwrap()), Some(entries));
    }

    /// Two chunks were written before this, and there is no reason they should
    /// stop reading.
    #[test]
    fn a_chunk_stored_before_compression_still_reads() {
        let entries = vec![message(5, 9, &[0x16, 0x03])];
        let plain = encode_chunk(HISTORY, &entries);
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
        let plain = encode_chunk(HISTORY, &entries);
        let stored = compress(&plain).expect("compress");
        assert!(
            stored.len() * 3 < plain.len(),
            "{} compressed to {}",
            plain.len(),
            stored.len()
        );
    }

    /// Several chunks are in flight at once and finish in whatever order they
    /// like. A recording assembled in the order they *arrive* would be
    /// silently out of order -- it would still decode, and every sequence in
    /// it would be wrong.
    #[tokio::test]
    async fn chunks_are_joined_in_key_order_however_they_arrive() {
        let keys: Vec<String> = (0..16).map(|index| format!("chunk-{index:02}")).collect();
        let joined = assemble(keys.clone(), |key| async move {
            // The later the key, the sooner it comes back: the arrival order
            // is exactly the reverse of the order the log needs.
            let index: u64 = key.trim_start_matches("chunk-").parse().unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20 - index)).await;
            Ok(format!("[{index}]").into_bytes())
        })
        .await
        .expect("assembled");

        let expected: String = (0..16).map(|index| format!("[{index}]")).collect();
        assert_eq!(String::from_utf8(joined).unwrap(), expected);
    }

    /// One chunk that will not come back fails the download rather than
    /// yielding the rest, which would be a recording with a hole in it that
    /// reads as whole.
    #[tokio::test]
    async fn a_chunk_that_fails_fails_the_whole_download() {
        let keys: Vec<String> = (0..8).map(|index| format!("chunk-{index}")).collect();
        let result = assemble(keys, |key| async move {
            if key.ends_with('5') {
                return Err("gone".into());
            }
            Ok(key.into_bytes())
        })
        .await;
        assert!(result.is_err());
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
