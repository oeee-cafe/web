use axum::extract::ws::Message;
use redis::AsyncCommands;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error};
use uuid::Uuid;

use crate::redis::RedisPool;

// One hour after the final live connection or stored operation.
const MESSAGE_HISTORY_TTL: u64 = 3600;
// v4 adds a durable history identity to every canonical envelope. This keeps
// positions from two Redis/history lifetimes from being compared as though
// they belonged to the same timeline (Drawpile's HistoryIndex invariant).
const MESSAGE_HISTORY_PREFIX: &str = "oeee:msg_history:v4:";
const MESSAGE_SEQ_PREFIX: &str = "oeee:msg_seq:v4:";
const MESSAGE_HISTORY_ID_PREFIX: &str = "oeee:msg_history_id:v4:";
// What the history weighs, and what it weighed just after the last checkpoint
// replaced it. Kept as counters beside the list because the alternative is
// summing every entry on the path of every drawing message.
const MESSAGE_BYTES_PREFIX: &str = "oeee:msg_bytes:v4:";
const RESET_BASE_PREFIX: &str = "oeee:msg_reset_base:v4:";
const CHAT_HISTORY_PREFIX: &str = "oeee:chat_history:v1:";
const MAX_CHAT_MESSAGES: usize = 100;

/// The most a room's canonical history may weigh.
///
/// This is a wall, not a trim. The history used to be `LTRIM`med to a message
/// count, which drops entries from the *front* -- and the front is where a
/// checkpoint's snapshots live. Everything after them is relative to them, so
/// trimming produced a history that replayed into a canvas nobody had ever
/// drawn, silently, an hour after the fact. Drawpile's `addMessage` returns
/// false when there is no space and the reset path disconnects with "History
/// limit exceeded" rather than truncate; refusing the newest message is a loss
/// its sender can see, where losing the oldest is a loss nobody can.
///
/// Auto-reset asks for a checkpoint far below this (see
/// `effective_auto_reset_bytes`), so reaching it means checkpoints have been
/// failing for a long time.
///
/// It has to clear the largest checkpoint that can exist -- 64 MiB, from
/// `MAX_SNAPSHOT_BYTES` and the largest seat count a session can be created
/// with -- by enough that a room can still draw after receiving one. A ceiling
/// a checkpoint could not fit under would refuse every message forever the
/// moment one landed, which is the one failure worse than the trimming this
/// replaced.
pub const MAX_HISTORY_BYTES: u64 = 192 * 1024 * 1024;

/// The most a checkpoint upload may weigh: two snapshots per seat of the
/// largest session, each at `MAX_SNAPSHOT_BYTES`. The websocket handler holds
/// a checkpoint in memory until its last snapshot arrives, so this is also the
/// most one connection can make the process buffer -- without it the count
/// limit alone allowed 510 snapshots of 4 MiB apiece.
pub const MAX_CHECKPOINT_BYTES: u64 =
    2 * (largest_session() as u64) * (super::protocol::MAX_SNAPSHOT_BYTES as u64);

/// A checkpoint has to fit, with room left to draw on top of it. Checked here
/// rather than trusted, because the three numbers it relates live in three
/// files and only this relationship between them keeps a room drawable.
const _: () = assert!(MAX_HISTORY_BYTES > MAX_CHECKPOINT_BYTES);

/// The largest seat count a session can be created with. Two snapshots per
/// participant is what a checkpoint is made of.
const fn largest_session() -> i32 {
    let choices = super::http_handlers::MAX_PARTICIPANTS_CHOICES;
    let mut largest = 0;
    let mut index = 0;
    while index < choices.len() {
        if choices[index] > largest {
            largest = choices[index];
        }
        index += 1;
    }
    largest
}

/// How much a room may add on top of its last checkpoint before the server
/// asks for a new one.
///
/// Measured as growth since the checkpoint, never as absolute size: our
/// checkpoints are PNGs of every participant's layers and weigh far more than
/// the strokes they replace, so a fixed ceiling would be over the moment a
/// checkpoint landed and every reset would immediately ask for another. This
/// is Drawpile's `autoResetThresholdBase`, which exists for exactly that
/// reason.
pub const AUTO_RESET_THRESHOLD_BYTES: u64 = 4 * 1024 * 1024;

/// The largest total this counts up to before refusing. Growth beyond what a
/// `u64` holds is not the failure mode worth guarding, but Lua numbers are
/// doubles and lose integer precision past 2^53, so the counter is kept in a
/// range where `tonumber` is exact.
const _: () = assert!(MAX_HISTORY_BYTES < (1u64 << 53));

/// Atomically assigns the next per-room sequence number, appends the message to
/// the history list, and publishes it to the room's Pub/Sub channel under the
/// position it was just given.
///
/// This is the single serialization point for drawing commands, modeled after
/// Drawpile's SessionHistory: because sequencing, storage, and broadcast happen
/// in one atomic step, the history replayed to late joiners and the live
/// Pub/Sub stream are guaranteed to present messages in the same canonical
/// order to every client.
///
/// A message that would take the history past `MAX_HISTORY_BYTES` is refused
/// outright -- no sequence number, no broadcast, nothing stored -- and the
/// caller is told. Nothing is ever dropped from the front to make room.
const SEQUENCE_AND_PUBLISH_SCRIPT: &str = r#"
local bytes = tonumber(redis.call('GET', KEYS[6]) or '0')
local grown = bytes + #ARGV[1]
if grown > tonumber(ARGV[3]) then
    -- Same arity as the success path: the caller decodes one shape, and a
    -- refusal that came back shorter would fail to parse instead of being read
    -- as the refusal it is.
    return {-1, '', bytes, 0, ''}
end
local seq = redis.call('INCR', KEYS[1])
local history_id = redis.call('GET', KEYS[5])
if not history_id then
    history_id = ARGV[5]
    redis.call('SET', KEYS[5], history_id)
end
local messages = redis.call('RPUSH', KEYS[2], tostring(seq) .. ':' .. ARGV[1])
redis.call('SET', KEYS[6], grown)
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[4]))
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[4]))
redis.call('EXPIRE', KEYS[4], tonumber(ARGV[4]))
redis.call('EXPIRE', KEYS[5], tonumber(ARGV[4]))
redis.call('EXPIRE', KEYS[6], tonumber(ARGV[4]))
redis.call('EXPIRE', KEYS[7], tonumber(ARGV[4]))
-- The room is active because a message just landed in it, so the activity
-- stamp is this call's business rather than a round trip of its own.
redis.call('SET', KEYS[8], ARGV[6])
redis.call('EXPIRE', KEYS[8], tonumber(ARGV[7]))
-- RoomBroadcast framing, written by hand because the payload must stay the
-- bytes the client sent: header, newline, payload. The empty field is
-- target_connection -- history messages go to the whole room.
local frame = '1|' .. ARGV[2] .. '||' .. seq .. '|' .. history_id .. '\n' .. ARGV[1]
redis.call('PUBLISH', KEYS[3], frame)
-- The same bytes into the archive buffer, prefixed with the moment they were
-- sequenced. Here rather than in the caller so that a message which got a
-- position is recorded with it, once and in that order: recording afterwards
-- leaves a window in which a message is canonical and unrecorded, which is
-- exactly the message a post-mortem wants. See collaborate::archive.
if ARGV[10] == '1' then
    redis.call('RPUSH', KEYS[9], ARGV[8] .. ':' .. frame)
    redis.call('EXPIRE', KEYS[9], tonumber(ARGV[9]))
end
-- Both auto-reset meters, read here because the write already had to touch
-- every key they are computed from. The caller decides on them without asking
-- again.
return {seq, history_id, grown, messages, redis.call('GET', KEYS[7]) or ''}
"#;

/// Compiled once: `Script::new` hashes the source, and this runs on every
/// drawing message a room produces.
static SEQUENCE_AND_PUBLISH: OnceLock<redis::Script> = OnceLock::new();

fn sequence_and_publish_script() -> &'static redis::Script {
    SEQUENCE_AND_PUBLISH.get_or_init(|| redis::Script::new(SEQUENCE_AND_PUBLISH_SCRIPT))
}

pub struct RedisMessageStore {
    pool: RedisPool,
}

fn history_key(room_uuid: Uuid) -> String {
    format!("{}{}", MESSAGE_HISTORY_PREFIX, room_uuid)
}

fn seq_key(room_uuid: Uuid) -> String {
    format!("{}{}", MESSAGE_SEQ_PREFIX, room_uuid)
}

fn history_id_key(room_uuid: Uuid) -> String {
    format!("{}{}", MESSAGE_HISTORY_ID_PREFIX, room_uuid)
}

fn bytes_key(room_uuid: Uuid) -> String {
    format!("{}{}", MESSAGE_BYTES_PREFIX, room_uuid)
}

fn reset_base_key(room_uuid: Uuid) -> String {
    format!("{}{}", RESET_BASE_PREFIX, room_uuid)
}

/// What the history weighs, against what the last checkpoint left behind.
///
/// Both meters are reported as growth since that checkpoint, because that is
/// what a late joiner has to replay on top of it and what a new checkpoint
/// would remove. Absolute size says nothing useful: most of it is the
/// checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistorySize {
    pub messages: usize,
    pub bytes: u64,
    pub base_messages: usize,
    pub base_bytes: u64,
}

impl HistorySize {
    pub fn messages_since_reset(&self) -> usize {
        self.messages.saturating_sub(self.base_messages)
    }

    pub fn bytes_since_reset(&self) -> u64 {
        self.bytes.saturating_sub(self.base_bytes)
    }
}

/// The size at which this room should be asked for a fresh checkpoint.
///
/// Drawpile's `SessionHistory::effectiveAutoResetThreshold`: the threshold sits
/// on top of whatever the last checkpoint weighs, and is clamped below the hard
/// ceiling so there is always room to ask before there is no room to draw.
pub fn effective_auto_reset_bytes(base_bytes: u64) -> u64 {
    (base_bytes + AUTO_RESET_THRESHOLD_BYTES).min(MAX_HISTORY_BYTES / 10 * 9)
}

/// What became of a message handed to the sequencer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequenced {
    /// Given this canonical position, with the history weighing `bytes` after.
    ///
    /// `size` is both auto-reset meters as of this write, read inside the same
    /// script rather than by a follow-up round trip: the caller checks them on
    /// every message, and they are only interesting the moment one lands.
    Stored {
        seq: u64,
        bytes: u64,
        size: HistorySize,
    },
    /// Refused. The history is at `MAX_HISTORY_BYTES` and nothing was stored,
    /// sequenced or broadcast.
    HistoryFull { bytes: u64 },
}

fn chat_history_key(room_uuid: Uuid) -> String {
    format!("{}{}", CHAT_HISTORY_PREFIX, room_uuid)
}

/// Splits a stored "{messages}:{bytes}" checkpoint base.
fn decode_reset_base(base: &str) -> Option<(usize, u64)> {
    let (messages, bytes) = base.split_once(':')?;
    Some((messages.parse().ok()?, bytes.parse().ok()?))
}

/// Splits a stored "{seq}:{payload}" entry. Returns None for malformed entries.
fn decode_entry(entry: &[u8]) -> Option<(u64, &[u8])> {
    let sep = entry.iter().position(|&b| b == b':')?;
    let seq = std::str::from_utf8(&entry[..sep]).ok()?.parse().ok()?;
    Some((seq, &entry[sep + 1..]))
}

impl RedisMessageStore {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    pub async fn append_chat_message(
        &self,
        room_uuid: Uuid,
        payload: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;
        let key = chat_history_key(room_uuid);
        redis::pipe()
            .rpush(&key, payload)
            .ltrim(&key, -(MAX_CHAT_MESSAGES as isize), -1)
            .expire(&key, MESSAGE_HISTORY_TTL as i64)
            .query_async::<()>(&mut *conn)
            .await?;
        Ok(())
    }

    pub async fn get_recent_chat(
        &self,
        room_uuid: Uuid,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;
        Ok(conn.lrange(chat_history_key(room_uuid), 0, -1).await?)
    }

    pub async fn sequence_and_publish(
        &self,
        room_uuid: Uuid,
        payload: &[u8],
        from_connection: &str,
        channel: &str,
    ) -> Result<Sequenced, Box<dyn std::error::Error + Send + Sync>> {
        self.sequence_and_publish_recording(room_uuid, payload, from_connection, channel, true)
            .await
    }

    /// The same, told whether this deployment keeps a recording.
    ///
    /// A room whose recording has nowhere private to go buffers nothing:
    /// entries that will never be flushed are memory held for an hour to no
    /// end. See `archive::bucket`.
    pub async fn sequence_and_publish_recording(
        &self,
        room_uuid: Uuid,
        payload: &[u8],
        from_connection: &str,
        channel: &str,
        recording: bool,
    ) -> Result<Sequenced, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await.map_err(|e| {
            error!("Failed to get Redis connection: {}", e);
            e
        })?;

        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let now = since_epoch.as_secs();
        // The archive stamps in milliseconds: drawing messages carry no time of
        // their own, and seconds are too coarse to pace a replay by.
        let now_millis = since_epoch.as_millis() as u64;

        let (seq, _history_id, bytes, messages, base): (i64, String, u64, usize, String) =
            sequence_and_publish_script()
                .key(seq_key(room_uuid))
                .key(history_key(room_uuid))
                .key(channel)
                .key(format!(
                    "{}{}",
                    super::redis_state::USER_ID_PREFIX,
                    room_uuid
                ))
                .key(history_id_key(room_uuid))
                .key(bytes_key(room_uuid))
                .key(reset_base_key(room_uuid))
                .key(format!(
                    "{}{}",
                    super::redis_state::ACTIVITY_PREFIX,
                    room_uuid
                ))
                .key(super::archive::buffer_key(room_uuid))
                .arg(payload)
                .arg(from_connection)
                .arg(MAX_HISTORY_BYTES)
                .arg(MESSAGE_HISTORY_TTL)
                .arg(Uuid::new_v4().to_string())
                .arg(now)
                .arg(super::redis_state::ACTIVITY_TTL)
                .arg(now_millis)
                .arg(super::archive::ARCHIVE_BUFFER_TTL)
                .arg(if recording { "1" } else { "0" })
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| {
                    error!("Failed to sequence message for room {}: {}", room_uuid, e);
                    e
                })?;

        if seq < 0 {
            return Ok(Sequenced::HistoryFull { bytes });
        }

        let seq = seq as u64;
        debug!(
            "Sequenced message {} for room {} ({} bytes, history now {} bytes)",
            seq,
            room_uuid,
            payload.len(),
            bytes
        );
        let (base_messages, base_bytes) = decode_reset_base(&base).unwrap_or((0, 0));
        Ok(Sequenced::Stored {
            seq,
            bytes,
            size: HistorySize {
                messages,
                bytes,
                base_messages,
                base_bytes,
            },
        })
    }

    /// The newest position a room has reached, or None if it has none yet.
    pub async fn current_sequence(
        &self,
        room_uuid: Uuid,
    ) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;
        Ok(conn.get::<_, Option<u64>>(seq_key(room_uuid)).await?)
    }

    /// Both meters at once, and the checkpoint they are measured from.
    pub async fn history_size(
        &self,
        room_uuid: Uuid,
    ) -> Result<HistorySize, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;
        let (messages, bytes, base): (usize, Option<u64>, Option<String>) = redis::pipe()
            .llen(history_key(room_uuid))
            .get(bytes_key(room_uuid))
            .get(reset_base_key(room_uuid))
            .query_async(&mut *conn)
            .await?;
        let (base_messages, base_bytes) = base
            .as_deref()
            .and_then(decode_reset_base)
            .unwrap_or((0, 0));
        Ok(HistorySize {
            messages,
            bytes: bytes.unwrap_or(0),
            base_messages,
            base_bytes,
        })
    }

    pub async fn get_history(
        &self,
        room_uuid: Uuid,
    ) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self
            .get_history_with_seqs(room_uuid)
            .await?
            .into_iter()
            .map(|(_, msg)| msg)
            .collect())
    }

    pub async fn get_history_with_seqs(
        &self,
        room_uuid: Uuid,
    ) -> Result<Vec<(u64, Message)>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;

        // Entries are RPUSHed, so the list is already in chronological order
        let entries: Vec<Vec<u8>> = conn.lrange(history_key(room_uuid), 0, -1).await?;

        let mut result = Vec::with_capacity(entries.len());
        for entry in &entries {
            match decode_entry(entry) {
                Some((seq, payload)) => result.push((seq, Message::Binary(payload.to_vec().into()))),
                None => debug!(
                    "Skipping malformed history entry in room {} ({} bytes)",
                    room_uuid,
                    entry.len()
                ),
            }
        }

        debug!(
            "Retrieved {} messages from Redis for room {}",
            result.len(),
            room_uuid
        );
        Ok(result)
    }

    /// Returns one atomic view of the history identity and ordered entries.
    /// The identity is created even for an empty room, so its first future
    /// operation belongs to the same timeline the connecting client saw.
    pub async fn get_history_snapshot(
        &self,
        room_uuid: Uuid,
    ) -> Result<(Uuid, Vec<(u64, Message)>), Box<dyn std::error::Error + Send + Sync>> {
        const SNAPSHOT_SCRIPT: &str = r#"
local history_id = redis.call('GET', KEYS[1])
if not history_id then
    history_id = ARGV[1]
    redis.call('SET', KEYS[1], history_id)
end
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[2], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[3], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[4], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[5], tonumber(ARGV[2]))
return {history_id, redis.call('LRANGE', KEYS[2], 0, -1)}
"#;
        let mut conn = self.pool.get().await?;
        let (history_id, entries): (String, Vec<Vec<u8>>) = redis::Script::new(SNAPSHOT_SCRIPT)
            .key(history_id_key(room_uuid))
            .key(history_key(room_uuid))
            .key(seq_key(room_uuid))
            .key(bytes_key(room_uuid))
            .key(reset_base_key(room_uuid))
            .arg(Uuid::new_v4().to_string())
            .arg(MESSAGE_HISTORY_TTL)
            .invoke_async(&mut *conn)
            .await?;
        let history_id = Uuid::parse_str(&history_id)?;
        let history = entries
            .iter()
            .filter_map(|entry| decode_entry(entry))
            .map(|(seq, payload)| (seq, Message::Binary(payload.to_vec().into())))
            .collect();
        Ok((history_id, history))
    }

    /// Keep a live session's canonical timeline alive even while nobody is
    /// drawing. Otherwise Redis expiry could silently restart sequence 1
    /// underneath clients that kept the WebSocket open.
    pub async fn touch_history(
        &self,
        room_uuid: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;
        let _: () = redis::pipe()
            .expire(history_id_key(room_uuid), MESSAGE_HISTORY_TTL as i64)
            .expire(history_key(room_uuid), MESSAGE_HISTORY_TTL as i64)
            .expire(seq_key(room_uuid), MESSAGE_HISTORY_TTL as i64)
            .expire(bytes_key(room_uuid), MESSAGE_HISTORY_TTL as i64)
            .expire(reset_base_key(room_uuid), MESSAGE_HISTORY_TTL as i64)
            // The session user ids are part of the same timeline: every
            // stroke in the history is stamped with one. Let the map lapse
            // while the history survives and the next joiner is handed id 1
            // again, and draws on whichever layers that id already owns.
            .expire(
                format!("{}{}", super::redis_state::USER_ID_PREFIX, room_uuid),
                MESSAGE_HISTORY_TTL as i64,
            )
            .query_async(&mut *conn)
            .await?;
        Ok(())
    }

    /// Atomically replaces the history prefix up to and including `base_seq`
    /// with the given reset snapshot payloads (Drawpile-style session reset).
    ///
    /// The snapshots represent the canonical canvas state at history position
    /// `base_seq`, so they are stored with that sequence number and placed
    /// before all surviving entries (seq > base_seq). Late joiners then replay
    /// [snapshots, messages after base_seq] and reconstruct the exact same
    /// state as live clients, who never see the reset at all.
    pub async fn apply_reset(
        &self,
        room_uuid: Uuid,
        base_seq: u64,
        payloads: &[Vec<u8>],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // The two counters are rebuilt here rather than adjusted, because the
        // checkpoint is what everything after it is measured against: its own
        // weight becomes the base that auto-reset compares growth to, and
        // getting that wrong in either direction means a room that either never
        // checkpoints again or checkpoints continuously.
        const APPLY_RESET_SCRIPT: &str = r#"
local base = tonumber(ARGV[1])
local current = tonumber(redis.call('GET', KEYS[2]) or '0')
if base > current then
    return redis.error_reply('reset base is ahead of canonical history')
end
local entries = redis.call('LRANGE', KEYS[1], 0, -1)
local kept = {}
local kept_bytes = 0
for i = 1, #entries do
    local e = entries[i]
    local sep = string.find(e, ':', 1, true)
    if sep then
        local seq = tonumber(string.sub(e, 1, sep - 1))
        if seq and seq > base then
            kept[#kept + 1] = e
            kept_bytes = kept_bytes + #e - sep
        end
    end
end
redis.call('DEL', KEYS[1])
local checkpoint_bytes = 0
for i = 3, #ARGV do
    redis.call('RPUSH', KEYS[1], ARGV[1] .. ':' .. ARGV[i])
    checkpoint_bytes = checkpoint_bytes + #ARGV[i]
end
for i = 1, #kept do
    redis.call('RPUSH', KEYS[1], kept[i])
end
local checkpoint_messages = #ARGV - 2
redis.call('SET', KEYS[3], checkpoint_bytes + kept_bytes)
redis.call('SET', KEYS[4], checkpoint_messages .. ':' .. checkpoint_bytes)
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[3], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[4], tonumber(ARGV[2]))
return {#kept, checkpoint_bytes}
"#;

        let mut conn = self.pool.get().await?;

        let script = redis::Script::new(APPLY_RESET_SCRIPT);
        let mut invocation = script.key(history_key(room_uuid));
        invocation.key(seq_key(room_uuid));
        invocation.key(bytes_key(room_uuid));
        invocation.key(reset_base_key(room_uuid));
        invocation.arg(base_seq).arg(MESSAGE_HISTORY_TTL);
        for payload in payloads {
            invocation.arg(payload.as_slice());
        }
        let (kept, checkpoint_bytes): (usize, u64) = invocation.invoke_async(&mut *conn).await?;

        debug!(
            "Applied session reset for room {} at seq {}: {} snapshots weighing {} bytes, \
             {} newer entries kept",
            room_uuid,
            base_seq,
            payloads.len(),
            checkpoint_bytes,
            kept
        );
        Ok(())
    }

    pub async fn cleanup_room(
        &self,
        room_uuid: Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.pool.get().await?;

        let deleted: usize = conn
            .del(&[
                history_key(room_uuid),
                seq_key(room_uuid),
                history_id_key(room_uuid),
                bytes_key(room_uuid),
                reset_base_key(room_uuid),
                chat_history_key(room_uuid),
                format!("{}{}", super::redis_state::USER_ID_PREFIX, room_uuid),
            ])
            .await?;
        if deleted > 0 {
            debug!("Cleaned up Redis message history for room {}", room_uuid);
        }

        Ok(())
    }

    /// How close a room is to the wall, for the sweep that used to trim it.
    ///
    /// It reports and does not act. Trimming was the bug: it deleted the oldest
    /// entries, which are a checkpoint's snapshots, and left every later
    /// message describing a canvas that no longer had a beginning. A room that
    /// shows up here is one whose checkpoints are not happening, and that is
    /// what wants fixing -- not the evidence of it.
    pub async fn history_pressure(
        &self,
        room_uuid: Uuid,
    ) -> Result<Option<HistorySize>, Box<dyn std::error::Error + Send + Sync>> {
        let size = self.history_size(room_uuid).await?;
        // Auto-reset asks for a checkpoint at nine tenths of the wall at the
        // very latest, so anything past that has already been asked and has
        // not been given one.
        Ok((size.bytes > effective_auto_reset_bytes(size.base_bytes)).then_some(size))
    }
}
