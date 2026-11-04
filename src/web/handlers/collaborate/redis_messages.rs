use axum::extract::ws::Message;
use redis::AsyncCommands;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, error, warn};
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
// A checkpoint's snapshots on their way into the history, one key per upload.
// Not room state: it exists for the moment between staging and the swap, and
// expires by itself if the process dies in that moment.
const RESET_STAGING_PREFIX: &str = "oeee:msg_reset_staging:v4:";
const RESET_STAGING_TTL: u64 = 300;
const CHAT_HISTORY_PREFIX: &str = "oeee:chat_history:v2:";
const MAX_CHAT_MESSAGES: usize = 100;

/// Messages read per `LRANGE` when a history is sent to somebody joining.
/// A checkpoint's snapshots, megabytes apiece, are read one a command.
///
/// The read is done outside Lua on purpose. Redis is shared with other apps
/// and runs one command at a time, and a reply built inside a script copies
/// every value into a Lua string and back: the same 68 MiB history that a
/// plain `LRANGE` returns in 38 ms held Redis for a second through `EVAL`.
const HISTORY_READ_BATCH: isize = 256;

/// How often a join reads the history in slices before it settles for one
/// script that cannot be interleaved. A checkpoint landing mid-read is what
/// makes a read come back torn, and checkpoints are minutes apart.
const HISTORY_READ_ATTEMPTS: usize = 3;

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
-- The history's size, which auto-reset is decided on, read here because the
-- write already had to touch every key it is computed from. The caller decides on them without asking
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
/// Messages and bytes are both reported as growth since that checkpoint,
/// because that is what a late joiner has to replay on top of it and what a
/// new checkpoint would remove. Absolute size says nothing useful: most of it
/// is the checkpoint.
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
    (base_bytes + AUTO_RESET_THRESHOLD_BYTES).min(URGENT_HISTORY_BYTES)
}

/// Past this a room needs a checkpoint whatever it costs.
///
/// A client cuts a checkpoint behind everything anybody can still undo, and
/// declines when that would free nothing yet: the room asks again later, by
/// which time reach has moved on. A room this close to `MAX_HISTORY_BYTES`
/// may not have a later -- at the ceiling it refuses every message, so reach
/// never moves -- and is told so, so that somebody cuts at the head instead,
/// freezing whatever undo that takes, rather than leaving the room unable to
/// draw at all.
pub const URGENT_HISTORY_BYTES: u64 = MAX_HISTORY_BYTES / 10 * 9;

pub fn needs_checkpoint_urgently(bytes: u64) -> bool {
    bytes > URGENT_HISTORY_BYTES
}

/// A history as a connecting client is sent it; see `get_history_since`.
#[derive(Debug)]
pub struct HistorySince {
    pub history_id: Uuid,
    /// The highest sequence in the history, whether or not it is sent.
    pub max_seq: u64,
    /// The position the client resumes after: what it asked for when that
    /// was on this history and not past its end, else zero.
    pub after_seq: u64,
    /// Every entry after `after_seq`, in order.
    pub entries: Vec<(u64, Message)>,
}

/// What became of a message handed to the sequencer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequenced {
    /// Given this canonical position, with the history weighing `bytes` after.
    ///
    /// `size` is the history's size as of this write, read inside the same
    /// script rather than by a follow-up round trip: the caller checks it on
    /// every message, and it is only interesting the moment one lands.
    Stored {
        seq: u64,
        bytes: u64,
        size: HistorySize,
    },
    /// Refused. The history is at `MAX_HISTORY_BYTES` and nothing was stored,
    /// sequenced or broadcast.
    HistoryFull { bytes: u64 },
}

/// Every key a room's drawing lives in: its history and where it stands, the
/// seats its marks are stamped with, and its recent chat. What `cleanup_room`
/// deletes, and what a pause (`collaborate::pause`) carries away and back.
pub(crate) fn room_keys(room_uuid: Uuid) -> Vec<String> {
    vec![
        history_key(room_uuid),
        seq_key(room_uuid),
        history_id_key(room_uuid),
        bytes_key(room_uuid),
        reset_base_key(room_uuid),
        chat_history_key(room_uuid),
        format!("{}{}", super::redis_state::USER_ID_PREFIX, room_uuid),
    ]
}

/// How long a room's drawing stays in Redis after its last touch.
pub(crate) const HISTORY_TTL: u64 = MESSAGE_HISTORY_TTL;

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
    let (seq, sep) = split_entry(entry)?;
    Some((seq, &entry[sep + 1..]))
}

/// The sequence of a stored entry and where its separator is.
fn split_entry(entry: &[u8]) -> Option<(u64, usize)> {
    let sep = entry.iter().position(|&b| b == b':')?;
    let seq = std::str::from_utf8(&entry[..sep]).ok()?.parse().ok()?;
    Some((seq, sep))
}

/// Where a joiner's history starts and ends, without reading it.
///
/// Returns the identity, the last sequence, the position the client resumes
/// after (zero for none), the index to read from, the list's length, and how
/// many snapshots stand at its front. That count is the checkpoint's own
/// record of itself (the base key, `{messages}:{bytes}`), not something
/// learned by reading the snapshots: each is a layer of canvas, megabytes,
/// and reading one inside a script copies it into Lua. The count only places
/// the guess. The guess is kept only if the entries either side of it
/// straddle the client's position, which in a list in sequence order is
/// enough for it to be right, and the read starts from the front otherwise.
const SINCE_BOUNDS_SCRIPT: &str = r#"
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
local function seq_at(index)
    local entry = redis.call('LINDEX', KEYS[2], index)
    if not entry then return nil end
    return tonumber(string.match(entry, '^(%d+):'))
end
local len = redis.call('LLEN', KEYS[2])
local leading = 0
local base = redis.call('GET', KEYS[5])
if base then leading = tonumber(string.match(base, '^(%d+):')) or 0 end
if leading > len then leading = len end
local max_seq = 0
if len > 0 then max_seq = seq_at(-1) or 0 end
local after = 0
local resume_seq = tonumber(ARGV[4])
if ARGV[3] == history_id and resume_seq <= max_seq then after = resume_seq end
local start = 0
if after > 0 and leading < len then
    local first_seq = seq_at(leading)
    if first_seq and after >= first_seq - 1 then
        local guess = leading + (after - first_seq + 1)
        local ok = guess <= len
        if ok and guess < len then ok = seq_at(guess) == after + 1 end
        if ok and guess == len then ok = max_seq == after end
        -- The list is in sequence order, so an entry at or below the client's
        -- position just before the guess puts everything before it there too.
        -- This is what makes the answer right whatever the base key says; it
        -- reads a snapshot only for a client standing exactly on the base.
        if ok and guess > 0 then ok = (seq_at(guess - 1) or 0) <= after end
        if ok then start = guess end
    end
end
return {history_id, max_seq, after, start, len, leading}
"#;

static SINCE_BOUNDS: OnceLock<redis::Script> = OnceLock::new();

fn since_bounds_script() -> &'static redis::Script {
    SINCE_BOUNDS.get_or_init(|| redis::Script::new(SINCE_BOUNDS_SCRIPT))
}

/// Whether the sequences a sliced read collected are one version of the
/// history, read from `start_at_head` (index 0) or from just after `after`.
///
/// One version is: as many entries as the bounds promised, ending at the
/// last sequence they promised, each one more than the last -- except a
/// checkpoint's snapshots, which share one sequence and stand at the front.
/// A checkpoint applied between two slices shifts every index, and the slice
/// read after it no longer continues the one before.
fn read_is_one_history(
    seqs: &[u64],
    start_at_head: bool,
    after: u64,
    max_seq: u64,
    expected: usize,
) -> bool {
    if seqs.len() != expected {
        return false;
    }
    let (Some(&first), Some(&last)) = (seqs.first(), seqs.last()) else {
        return true;
    };
    if last != max_seq || (!start_at_head && first != after + 1) {
        return false;
    }
    let mut in_checkpoint = start_at_head;
    seqs.windows(2).all(|pair| {
        if pair[1] == pair[0] + 1 {
            in_checkpoint = false;
            true
        } else {
            in_checkpoint && pair[1] == pair[0]
        }
    })
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

    /// Messages and bytes at once, and the checkpoint they are measured from.
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
                Some((seq, payload)) => {
                    result.push((seq, Message::Binary(payload.to_vec().into())))
                }
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

    /// The history a connecting client has to be sent: its identity, its
    /// current position, and the entries after the position the client asked
    /// to resume from -- or all of them, when that position is not on this
    /// history or is past its end.
    ///
    /// Read from Redis at the resume position rather than whole and skipped
    /// in the process. A history is the last checkpoint plus up to the
    /// auto-reset threshold of messages, and a reconnecting client usually
    /// wants the last few of them; every resume used to copy all of it out
    /// of Redis to throw most away. The list is dense in sequence after a
    /// checkpoint's snapshots, which share the base sequence at the front and
    /// whose number the checkpoint's base key records, so the index of the
    /// first entry to send is arithmetic -- checked against its neighbours
    /// before it is trusted, and the whole list read if the check fails.
    ///
    /// Only that arithmetic is a script; the entries are read after it, in
    /// slices, by plain commands (see [`HISTORY_READ_BATCH`]). A checkpoint
    /// rewriting the list between the slices is caught by the sequence
    /// numbers not running on as they must, and the read is taken again.
    pub async fn get_history_since(
        &self,
        room_uuid: Uuid,
        resume: Option<(Uuid, u64)>,
    ) -> Result<HistorySince, Box<dyn std::error::Error + Send + Sync>> {
        for _ in 0..HISTORY_READ_ATTEMPTS {
            if let Some(since) = self.read_history_since(room_uuid, resume).await? {
                return Ok(since);
            }
        }
        warn!(
            "History of room {} changed under {} sliced reads; reading it in one script",
            room_uuid, HISTORY_READ_ATTEMPTS
        );
        self.get_history_since_atomically(room_uuid, resume).await
    }

    /// One attempt at the sliced read: None when what came back is not one
    /// version of the history.
    async fn read_history_since(
        &self,
        room_uuid: Uuid,
        resume: Option<(Uuid, u64)>,
    ) -> Result<Option<HistorySince>, Box<dyn std::error::Error + Send + Sync>> {
        let (resume_history, resume_seq) = match resume {
            Some((history_id, seq)) => (history_id.to_string(), seq),
            None => (String::new(), 0),
        };
        let mut conn = self.pool.get().await?;
        let (history_id, max_seq, after_seq, start, len, leading): (
            String,
            u64,
            u64,
            isize,
            isize,
            isize,
        ) = since_bounds_script()
            .key(history_id_key(room_uuid))
            .key(history_key(room_uuid))
            .key(seq_key(room_uuid))
            .key(bytes_key(room_uuid))
            .key(reset_base_key(room_uuid))
            .arg(Uuid::new_v4().to_string())
            .arg(MESSAGE_HISTORY_TTL)
            .arg(resume_history)
            .arg(resume_seq)
            .invoke_async(&mut *conn)
            .await?;

        let mut raw: Vec<Vec<u8>> = Vec::with_capacity((len - start).max(0) as usize);
        let mut index = start;
        while index < len {
            let last = if index < leading {
                index
            } else {
                (index + HISTORY_READ_BATCH).min(len) - 1
            };
            let slice: Vec<Vec<u8>> = conn.lrange(history_key(room_uuid), index, last).await?;
            if slice.is_empty() {
                // Shorter than it was a moment ago: a checkpoint replaced it.
                return Ok(None);
            }
            index += slice.len() as isize;
            raw.extend(slice);
        }

        let mut seqs = Vec::with_capacity(raw.len());
        let mut entries = Vec::with_capacity(raw.len());
        for entry in raw {
            let Some((seq, sep)) = split_entry(&entry) else {
                return Ok(None);
            };
            seqs.push(seq);
            if seq > after_seq {
                // The payload is a view into the entry Redis sent, not a copy
                // of it: a joiner's replay is the whole history.
                let payload = axum::body::Bytes::from(entry).slice(sep + 1..);
                entries.push((seq, Message::Binary(payload)));
            }
        }
        let expected = (len - start).max(0) as usize;
        if !read_is_one_history(&seqs, start == 0, after_seq, max_seq, expected) {
            return Ok(None);
        }
        Ok(Some(HistorySince {
            history_id: Uuid::parse_str(&history_id)?,
            max_seq,
            after_seq,
            entries,
        }))
    }

    /// The same answer from one script, which nothing can interleave with.
    /// What a join falls back on when the sliced read keeps coming back torn;
    /// it holds Redis for as long as the whole history takes to copy through
    /// Lua, so it is not the way in.
    async fn get_history_since_atomically(
        &self,
        room_uuid: Uuid,
        resume: Option<(Uuid, u64)>,
    ) -> Result<HistorySince, Box<dyn std::error::Error + Send + Sync>> {
        const SINCE_SCRIPT: &str = r#"
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
local function seq_at(index)
    local entry = redis.call('LINDEX', KEYS[2], index)
    if not entry then return nil end
    return tonumber(string.match(entry, '^(%d+):'))
end
local len = redis.call('LLEN', KEYS[2])
local max_seq = 0
if len > 0 then max_seq = seq_at(-1) or 0 end
local after = 0
local resume_seq = tonumber(ARGV[4])
if ARGV[3] == history_id and resume_seq <= max_seq then after = resume_seq end
return {history_id, tostring(max_seq), tostring(after), redis.call('LRANGE', KEYS[2], 0, -1)}
"#;
        let (resume_history, resume_seq) = match resume {
            Some((history_id, seq)) => (history_id.to_string(), seq),
            None => (String::new(), 0),
        };
        let mut conn = self.pool.get().await?;
        let (history_id, max_seq, after_seq, entries): (String, String, String, Vec<Vec<u8>>) =
            redis::Script::new(SINCE_SCRIPT)
                .key(history_id_key(room_uuid))
                .key(history_key(room_uuid))
                .key(seq_key(room_uuid))
                .key(bytes_key(room_uuid))
                .key(reset_base_key(room_uuid))
                .arg(Uuid::new_v4().to_string())
                .arg(MESSAGE_HISTORY_TTL)
                .arg(resume_history)
                .arg(resume_seq)
                .invoke_async(&mut *conn)
                .await?;
        let after_seq: u64 = after_seq.parse()?;
        let entries = entries
            .iter()
            .filter_map(|entry| decode_entry(entry))
            .filter(|(seq, _)| *seq > after_seq)
            .map(|(seq, payload)| (seq, Message::Binary(payload.to_vec().into())))
            .collect();
        Ok(HistorySince {
            history_id: Uuid::parse_str(&history_id)?,
            max_seq: max_seq.parse()?,
            after_seq,
            entries,
        })
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
        //
        // The snapshots never pass through Lua. A checkpoint is up to 64 MiB
        // of them, and the script that used to take them as arguments and
        // rebuild the list from a full `LRANGE` held this shared Redis for
        // most of a second doing it. They are pushed first, one plain command
        // each, onto a key of this upload's own; the script then appends only
        // the messages that outlive the checkpoint -- small, and found by
        // bisection -- and swaps the staged list in with `RENAME`, letting the
        // old one go with `UNLINK`. Readers see the old history or the new
        // one, never a list between them.
        const APPLY_RESET_SCRIPT: &str = r#"
local base = tonumber(ARGV[1])
local current = tonumber(redis.call('GET', KEYS[2]) or '0')
if base > current then
    return redis.error_reply('reset base is ahead of canonical history')
end
if redis.call('LLEN', KEYS[5]) ~= tonumber(ARGV[3]) then
    return redis.error_reply('checkpoint snapshots were not all staged')
end
-- A checkpoint is cut behind the head, so it can be cut behind the one
-- before it too. Then the older one's snapshots, which carry its base, would
-- be kept above the new ones and stamp an older canvas over the newer one.
local first = redis.call('LINDEX', KEYS[1], 0)
if first then
    local sep = string.find(first, ':', 1, true)
    if sep and string.byte(first, sep + 1) == 2
        and tonumber(string.sub(first, 1, sep - 1)) > base then
        return redis.error_reply('reset base is behind the last checkpoint')
    end
end
-- The list is in sequence order (the sequencer appends, and a checkpoint puts
-- its base in front of what is above it), so the first entry past the base
-- is a bisection away.
local function seq_at(index)
    local entry = redis.call('LINDEX', KEYS[1], index)
    if not entry then return nil end
    return tonumber(string.match(entry, '^(%d+):'))
end
local low, high = 0, redis.call('LLEN', KEYS[1])
local len = high
while low < high do
    local mid = math.floor((low + high) / 2)
    local seq = seq_at(mid)
    if seq == nil or seq <= base then low = mid + 1 else high = mid end
end
local kept = 0
local kept_bytes = 0
if low < len then
    local entries = redis.call('LRANGE', KEYS[1], low, -1)
    for i = 1, #entries do
        local e = entries[i]
        local sep = string.find(e, ':', 1, true)
        if sep then
            local seq = tonumber(string.sub(e, 1, sep - 1))
            if seq and seq > base then
                redis.call('RPUSH', KEYS[5], e)
                kept = kept + 1
                kept_bytes = kept_bytes + #e - sep
            end
        end
    end
end
redis.call('UNLINK', KEYS[1])
if redis.call('EXISTS', KEYS[5]) == 1 then
    redis.call('RENAME', KEYS[5], KEYS[1])
end
local checkpoint_bytes = tonumber(ARGV[4])
redis.call('SET', KEYS[3], checkpoint_bytes + kept_bytes)
redis.call('SET', KEYS[4], ARGV[3] .. ':' .. ARGV[4])
redis.call('EXPIRE', KEYS[1], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[3], tonumber(ARGV[2]))
redis.call('EXPIRE', KEYS[4], tonumber(ARGV[2]))
return {kept, checkpoint_bytes}
"#;
        static APPLY_RESET: OnceLock<redis::Script> = OnceLock::new();

        let staging = format!("{}{}:{}", RESET_STAGING_PREFIX, room_uuid, Uuid::new_v4());
        let mut conn = self.pool.get().await?;

        let applied: Result<(usize, u64), Box<dyn std::error::Error + Send + Sync>> = async {
            let prefix = format!("{base_seq}:");
            for payload in payloads {
                let mut entry = Vec::with_capacity(prefix.len() + payload.len());
                entry.extend_from_slice(prefix.as_bytes());
                entry.extend_from_slice(payload);
                conn.rpush::<_, _, ()>(&staging, entry).await?;
                conn.expire::<_, ()>(&staging, RESET_STAGING_TTL as i64)
                    .await?;
            }
            let checkpoint_bytes: u64 = payloads.iter().map(|p| p.len() as u64).sum();
            Ok(APPLY_RESET
                .get_or_init(|| redis::Script::new(APPLY_RESET_SCRIPT))
                .key(history_key(room_uuid))
                .key(seq_key(room_uuid))
                .key(bytes_key(room_uuid))
                .key(reset_base_key(room_uuid))
                .key(&staging)
                .arg(base_seq)
                .arg(MESSAGE_HISTORY_TTL)
                .arg(payloads.len())
                .arg(checkpoint_bytes)
                .invoke_async(&mut *conn)
                .await?)
        }
        .await;

        let (kept, checkpoint_bytes) = match applied {
            Ok(applied) => applied,
            Err(e) => {
                // Refused or cut short: what was staged is nobody's.
                let _: Result<(), _> = redis::cmd("UNLINK")
                    .arg(&staging)
                    .query_async(&mut *conn)
                    .await;
                return Err(e);
            }
        };

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

        // UNLINK, not DEL: a history can be large, and DEL frees it on the
        // one thread every other client of this shared Redis is waiting on.
        let deleted: usize = redis::cmd("UNLINK")
            .arg(room_keys(room_uuid))
            .query_async(&mut *conn)
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

#[cfg(test)]
mod urgency_tests {
    use super::*;

    /// A client may decline a checkpoint that would take somebody's undo, and
    /// the room asks again later. A room whose last checkpoint is so heavy
    /// that auto-reset is pinned at its clamp may have no later, so it has to
    /// be told it is urgent the moment it is asked at all.
    #[test]
    fn a_room_asked_at_the_clamp_is_told_it_is_urgent() {
        let pinned = effective_auto_reset_bytes(URGENT_HISTORY_BYTES);
        assert!(needs_checkpoint_urgently(pinned + 1));
        // An ordinary room is not, and may wait for a checkpoint that costs
        // nobody anything.
        assert!(!needs_checkpoint_urgently(
            effective_auto_reset_bytes(0) + 1
        ));
    }
}

#[cfg(test)]
mod sliced_read_tests {
    use super::read_is_one_history;

    #[test]
    fn a_whole_history_with_a_checkpoint_at_its_head_is_one_history() {
        assert!(read_is_one_history(&[3, 3, 3, 3, 4, 5], true, 0, 5, 6));
        assert!(read_is_one_history(&[1, 2, 3], true, 0, 3, 3));
        assert!(read_is_one_history(&[], true, 0, 0, 0));
    }

    #[test]
    fn a_resume_starts_one_past_where_the_client_stands() {
        assert!(read_is_one_history(&[5, 6], false, 4, 6, 2));
        assert!(!read_is_one_history(&[6, 7], false, 4, 7, 2));
        assert!(read_is_one_history(&[], false, 6, 6, 0));
    }

    /// A checkpoint applied between two slices shifts the second slice: it
    /// starts somewhere other than where the first one stopped.
    #[test]
    fn a_read_torn_by_a_checkpoint_is_caught() {
        // Old head [2, 2, 3, 4], then a slice of the new list [7, 7, 8, ...].
        assert!(!read_is_one_history(&[2, 2, 3, 4, 7, 8], true, 0, 8, 6));
        // A second snapshot group in the middle is not a checkpoint's head.
        assert!(!read_is_one_history(&[3, 3, 4, 4, 5], true, 0, 5, 5));
        // Shorter than the bounds promised: the list shrank under the read.
        assert!(!read_is_one_history(&[3, 3, 4], true, 0, 5, 5));
        // Ends short of where the bounds said the history reached.
        assert!(!read_is_one_history(&[3, 3, 4], true, 0, 5, 3));
        // A snapshot pair is only a checkpoint at the front of the list.
        assert!(!read_is_one_history(&[5, 5, 6], false, 4, 6, 3));
    }
}
