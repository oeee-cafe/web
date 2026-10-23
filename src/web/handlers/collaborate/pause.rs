//! Pausing a room nobody is drawing in, rather than ending it.
//!
//! A room's drawing lives in Redis, and Redis lets it go an hour after the
//! last connection: a room that ran out of activity before its owner saved it
//! used to lose the drawing for good, with only the owner able to have saved
//! it. Now an idle room with an unsaved drawing is *paused*: its keys are
//! copied out of Redis, stored as one object in the private bucket, and the
//! session stays open. The first person allowed in puts them back before they
//! are sent the history, and the room carries on
//! from exactly where it stood -- the same history id, so a page that was open
//! when it paused resumes rather than replays.
//!
//! A paused room ends for good after [`PAUSED_RETENTION`]. A room with nothing
//! drawn in it is not worth keeping and ends as it always did.
//!
//! ## Ordering
//!
//! Pausing and resuming both lock the session row, which every join locks
//! too (`track_participant_with_capacity_check`, `FOR UPDATE`). So nobody
//! joins a room halfway through being paused, and the first to arrive at a
//! paused one puts it back while the others wait their turn behind the lock
//! and find it already back. The Redis keys are deleted before the pause
//! commits: deleted after, a join waiting on the lock would restore them and
//! then watch them be deleted.
//!
//! The lock is `FOR NO KEY UPDATE`, not `FOR UPDATE`. A pause flushes the
//! room's recording while it holds the row, and that write goes through the
//! pool, on a connection of its own: a first upsert into
//! `collaborative_session_recordings` checks its foreign key with `FOR KEY
//! SHARE` on this same row, which `FOR UPDATE` blocks. The pause then waited
//! on the flush and the flush on the pause, a cycle Postgres cannot see
//! because half of it is in this process -- and every join to the room, and
//! the sweep behind it, queued up after them for good. `FOR NO KEY UPDATE`
//! lets a foreign key check through and still excludes every join
//! (`FOR UPDATE`) and every other pause or resume.
//!
//! ## Sharing Redis
//!
//! Redis is shared with other apps on the host and runs one command at a
//! time, so nothing here is one large command. A history is copied out in
//! slices of [`LIST_BATCH`] entries and put back the same way, where `DUMP`
//! and `RESTORE` would each have serialised a whole history -- up to the
//! history ceiling, for a room whose checkpoints had been failing -- with
//! every other client waiting. Keys are let go with `UNLINK`, which frees
//! their memory off the main thread. And the copy is in a format of our own
//! rather than `DUMP`'s, which a Redis older than the one that wrote it
//! cannot read.
//!
//! The copy is not one atomic read, and need not be: nobody is in a pausing
//! room (its page was sent away first) or a resuming one (joins wait on the
//! session row), so its keys do not change underneath.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Duration;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::preview::{inspect_image, PreviewStore, StoredPreview};
use super::redis_messages::{self, HISTORY_TTL};
use super::websocket::{send_away_everyone, Departure};
use crate::web::state::AppState;

/// How long a paused room waits for somebody to come back to it.
pub const PAUSED_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// When a room paused at `paused_at` ends if nobody comes back, give or take
/// the sweep that ends it: what the lobby and the staff list tell people.
pub fn ends_at(paused_at: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    paused_at + chrono::Duration::from_std(PAUSED_RETENTION).expect("a week fits")
}

/// What a stored pause begins with, and its layout version: 2 holds each key
/// by its contents. 1, written for a few minutes before it, held `DUMP`s, and
/// is still read in case a room was paused then.
const MAGIC: &[u8] = b"OEEEPAUSE\x02";
const MAGIC_DUMPS: &[u8] = b"OEEEPAUSE\x01";

/// Entries read or written per command, for a list: small enough that no
/// command keeps Redis from its other clients for long, whatever the entries
/// hold (a checkpoint's snapshots are the largest, at a few megabytes).
const LIST_BATCH: isize = 256;

/// A key's contents, by the type it has. A room keeps no other kind.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    String(Vec<u8>),
    List(Vec<Vec<u8>>),
    Hash(Vec<(Vec<u8>, Vec<u8>)>),
}

/// What a stored pause holds: each key, and what was in it.
type Kept = Vec<(String, Value)>;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Where a paused room's state is kept in the bucket.
fn object_key(room_uuid: Uuid) -> String {
    format!("paused/{room_uuid}")
}

/// Where its lobby preview is kept, beside it and apart from it: a card asks
/// for the picture every time the lobby repaints, and the state is megabytes
/// that would have to be fetched and unpacked to get at thirty kilobytes.
fn preview_object_key(room_uuid: Uuid) -> String {
    format!("paused/{room_uuid}.preview")
}

/// The keys a pause carries: the room's drawing, and its recording's chat
/// buffer, whose lifetime is a day and so shorter than a pause.
fn carried_keys(room_uuid: Uuid) -> Vec<String> {
    let mut keys = redis_messages::room_keys(room_uuid);
    keys.push(super::archive::chat_buffer_key(room_uuid));
    keys
}

fn put_bytes(raw: &mut Vec<u8>, bytes: &[u8]) {
    raw.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    raw.extend_from_slice(bytes);
}

/// `[magic][count:2]`, then each key `[name len:2][name][kind:1]` and its
/// contents: a string `[len:4][bytes]`, a list `[count:4]` of those, a hash
/// `[count:4]` of field-value pairs of them. The whole compressed.
fn encode(kept: &Kept) -> Result<Vec<u8>, Error> {
    let mut raw = Vec::from(MAGIC);
    raw.extend_from_slice(&(kept.len() as u16).to_le_bytes());
    for (name, value) in kept {
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(name.as_bytes());
        match value {
            Value::String(bytes) => {
                raw.push(0);
                put_bytes(&mut raw, bytes);
            }
            Value::List(entries) => {
                raw.push(1);
                raw.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for entry in entries {
                    put_bytes(&mut raw, entry);
                }
            }
            Value::Hash(pairs) => {
                raw.push(2);
                raw.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
                for (field, value) in pairs {
                    put_bytes(&mut raw, field);
                    put_bytes(&mut raw, value);
                }
            }
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

/// A stored pause read back: what it kept, or, from the first layout, the
/// `DUMP`s it kept.
enum Stored {
    Kept(Kept),
    Dumps(Vec<(String, Vec<u8>)>),
}

struct Reader<'a> {
    raw: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], Error> {
        let slice = self
            .raw
            .get(self.at..self.at + len)
            .ok_or("a paused room cut short")?;
        self.at += len;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<usize, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?) as usize)
    }
    fn u32(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?) as usize)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let len = self.u32()?;
        Ok(self.take(len)?.to_vec())
    }
    fn name(&mut self) -> Result<String, Error> {
        let len = self.u16()?;
        Ok(String::from_utf8(self.take(len)?.to_vec())?)
    }
}

fn decode(stored: &[u8]) -> Result<Stored, Error> {
    let mut raw = Vec::new();
    ZlibDecoder::new(stored).read_to_end(&mut raw)?;
    let dumps = raw.starts_with(MAGIC_DUMPS);
    if !dumps && !raw.starts_with(MAGIC) {
        return Err("not a paused room".into());
    }
    let mut reader = Reader {
        raw: &raw,
        at: MAGIC.len(),
    };
    let count = reader.u16()?;
    if dumps {
        let mut kept = Vec::with_capacity(count);
        for _ in 0..count {
            kept.push((reader.name()?, reader.bytes()?));
        }
        return Ok(Stored::Dumps(kept));
    }
    let mut kept = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.name()?;
        let value = match reader.take(1)?[0] {
            0 => Value::String(reader.bytes()?),
            1 => {
                let len = reader.u32()?;
                let mut entries = Vec::with_capacity(len);
                for _ in 0..len {
                    entries.push(reader.bytes()?);
                }
                Value::List(entries)
            }
            2 => {
                let len = reader.u32()?;
                let mut pairs = Vec::with_capacity(len);
                for _ in 0..len {
                    pairs.push((reader.bytes()?, reader.bytes()?));
                }
                Value::Hash(pairs)
            }
            kind => return Err(format!("a paused key of unknown kind {kind}").into()),
        };
        kept.push((name, value));
    }
    Ok(Stored::Kept(kept))
}

/// Copies the keys out, a slice at a time. Keys that do not exist are left
/// out; a kind a room never keeps is an error, not a key silently dropped.
async fn copy_out(state: &AppState, keys: &[String]) -> Result<Kept, Error> {
    let mut conn = state.redis_pool.get().await?;
    let mut kept = Vec::new();
    for key in keys {
        let kind: String = redis::cmd("TYPE").arg(key).query_async(&mut *conn).await?;
        let value = match kind.as_str() {
            "none" => continue,
            "string" => Value::String(redis::cmd("GET").arg(key).query_async(&mut *conn).await?),
            "hash" => Value::Hash(
                redis::cmd("HGETALL")
                    .arg(key)
                    .query_async(&mut *conn)
                    .await?,
            ),
            "list" => {
                let mut entries: Vec<Vec<u8>> = Vec::new();
                loop {
                    let start = entries.len() as isize;
                    let slice: Vec<Vec<u8>> = redis::cmd("LRANGE")
                        .arg(key)
                        .arg(start)
                        .arg(start + LIST_BATCH - 1)
                        .query_async(&mut *conn)
                        .await?;
                    let done = (slice.len() as isize) < LIST_BATCH;
                    entries.extend(slice);
                    if done {
                        break;
                    }
                }
                Value::List(entries)
            }
            other => {
                return Err(
                    format!("room key {key} is a {other}, which a pause does not carry").into(),
                )
            }
        };
        kept.push((key.clone(), value));
    }
    Ok(kept)
}

/// Lets keys go without freeing them on Redis's one thread.
async fn unlink(state: &AppState, keys: &[String]) -> Result<(), Error> {
    let mut conn = state.redis_pool.get().await?;
    let _: () = redis::cmd("UNLINK")
        .arg(keys)
        .query_async(&mut *conn)
        .await?;
    Ok(())
}

// Where the object goes. The private bucket in a deployment; in the tests,
// which have no bucket to reach, a map in memory standing in for it.

#[cfg(not(test))]
async fn put_object(state: &AppState, key: &str, body: Vec<u8>) -> Result<(), Error> {
    let bucket = super::archive::bucket(&state.config).ok_or("no private bucket configured")?;
    super::archive::s3_client(&state.config)
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_type("application/octet-stream")
        .body(aws_sdk_s3::primitives::ByteStream::from(body))
        .send()
        .await?;
    Ok(())
}

#[cfg(not(test))]
async fn get_object(state: &AppState, key: &str) -> Result<Vec<u8>, Error> {
    let bucket = super::archive::bucket(&state.config).ok_or("no private bucket configured")?;
    let object = super::archive::s3_client(&state.config)
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await?;
    Ok(object.body.collect().await?.into_bytes().to_vec())
}

#[cfg(not(test))]
async fn delete_object(state: &AppState, key: &str) -> Result<(), Error> {
    let bucket = super::archive::bucket(&state.config).ok_or("no private bucket configured")?;
    super::archive::s3_client(&state.config)
        .delete_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await?;
    Ok(())
}

#[cfg(test)]
static OBJECTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>> =
    std::sync::LazyLock::new(Default::default);

#[cfg(test)]
async fn put_object(_: &AppState, key: &str, body: Vec<u8>) -> Result<(), Error> {
    OBJECTS.lock().unwrap().insert(key.to_string(), body);
    Ok(())
}

#[cfg(test)]
async fn get_object(_: &AppState, key: &str) -> Result<Vec<u8>, Error> {
    Ok(OBJECTS
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .ok_or("no such paused room")?)
}

#[cfg(test)]
async fn delete_object(_: &AppState, key: &str) -> Result<(), Error> {
    OBJECTS.lock().unwrap().remove(key);
    Ok(())
}

/// Whether a stored pause exists for a room, for the tests.
#[cfg(test)]
pub(crate) fn is_stored(room_uuid: Uuid) -> bool {
    OBJECTS.lock().unwrap().contains_key(&object_key(room_uuid))
}

/// Whether a paused room's preview is kept in the bucket, for the tests.
#[cfg(test)]
pub(crate) fn preview_is_stored(room_uuid: Uuid) -> bool {
    OBJECTS
        .lock()
        .unwrap()
        .contains_key(&preview_object_key(room_uuid))
}

/// Whether a room has anything drawn in it: a position past the start.
async fn has_drawing(state: &AppState, room_uuid: Uuid) -> Result<bool, Error> {
    let store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
    Ok(store.current_sequence(room_uuid).await?.unwrap_or(0) > 0)
}

/// What an idle room became.
#[derive(Debug, PartialEq, Eq)]
pub enum Idled {
    /// Paused: its drawing is in the bucket, waiting.
    Paused,
    /// Nothing drawn and nothing to keep; the caller ends it as before.
    NothingToKeep,
    /// Somebody joined, or the room ended, while this was deciding.
    LeftAlone,
}

/// Takes an open, unpaused room's row for a pause; false when it is not one.
///
/// `FOR NO KEY UPDATE`, never `FOR UPDATE`: see "Ordering" above.
pub(crate) async fn lock_open_room(
    tx: &mut sqlx::PgConnection,
    room_uuid: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT id FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL AND paused_at IS NULL
        FOR NO KEY UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(tx)
    .await?
    .is_some())
}

/// Takes an open room's row for a resume; true when it is paused.
pub(crate) async fn lock_paused_room(
    tx: &mut sqlx::PgConnection,
    room_uuid: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar!(
        r#"
        SELECT paused_at IS NOT NULL AS "paused!" FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        FOR NO KEY UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(tx)
    .await?
        == Some(true))
}

/// Pauses an idle room with a drawing in it.
///
/// Anybody still connected is told it paused (close code 4002) -- an idle
/// room with its page open is the same as an empty one, and their page offers
/// to resume it. An error leaves the room exactly as it was, to be tried again
/// on the next sweep: nothing is deleted from Redis until the object is
/// stored.
pub async fn pause_idle_room(state: &AppState, room_uuid: Uuid) -> Result<Idled, Error> {
    // Without a private bucket there is nowhere to put it, and the room ends
    // as it did before pausing existed.
    #[cfg(not(test))]
    if super::archive::bucket(&state.config).is_none() {
        return Ok(Idled::NothingToKeep);
    }
    if !has_drawing(state, room_uuid).await? {
        return Ok(Idled::NothingToKeep);
    }

    let mut tx = state.db_pool.begin().await?;
    if !lock_open_room(&mut *tx, room_uuid).await? {
        return Ok(Idled::LeftAlone);
    }

    // The page open on it goes first, so nothing is drawn into the room
    // while it is being carried away.
    if send_away_everyone(state, room_uuid, Departure::Paused).await > 0 {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Recorded before it goes: the recording's buffers outlive an hour,
    // not a week.
    super::archive::flush_room(state, room_uuid).await;

    let keys = carried_keys(room_uuid);
    let kept = copy_out(state, &keys).await?;
    let body = encode(&kept)?;
    let size = body.len();
    put_object(state, &object_key(room_uuid), body).await?;
    let preview = keep_preview(state, room_uuid).await;

    unlink(state, &keys).await?;
    for cleaned in [
        state.redis_state.cleanup_room_state(room_uuid).await,
        PreviewStore::new(state.redis_pool.clone())
            .cleanup(room_uuid)
            .await
            .map_err(|e| -> Error { format!("{e}").into() }),
    ] {
        if let Err(e) = cleaned {
            warn!("Failed to clear a pausing room {room_uuid}'s state: {e}");
        }
    }

    let paused = async {
        sqlx::query!(
            "UPDATE collaborative_sessions SET paused_at = NOW(), paused_preview_version = $2 WHERE id = $1",
            room_uuid,
            preview.as_ref().map(|preview| preview.version as i64),
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            r#"
            UPDATE collaborative_sessions_participants
            SET is_active = false, left_at = NOW()
            WHERE session_id = $1 AND is_active
            "#,
            room_uuid
        )
        .execute(&mut *tx)
        .await?;
        Ok::<_, sqlx::Error>(())
    }
    .await;
    match paused {
        Ok(()) => tx.commit().await?,
        Err(e) => {
            // The drawing is out of Redis and the room is not marked paused:
            // put it back, so the room is as it was.
            drop(tx);
            if let Some(preview) = &preview {
                put_back_preview(state, room_uuid, preview).await;
            }
            if let Err(restore) = restore(state, room_uuid, Stored::Kept(kept)).await {
                error!(
                    "Room {room_uuid} could not be marked paused ({e}) nor put back ({restore}); its drawing is in the bucket at {}",
                    object_key(room_uuid)
                );
            }
            return Err(e.into());
        }
    }
    info!("Paused room {room_uuid}: {} keys, {size} bytes", kept.len());
    Ok(Idled::Paused)
}

/// Puts the keys back, a slice at a time, alive for as long as a room's
/// history is.
async fn restore(state: &AppState, room_uuid: Uuid, stored: Stored) -> Result<(), Error> {
    let mut conn = state.redis_pool.get().await?;
    let ttl = HISTORY_TTL as i64;
    match stored {
        Stored::Kept(kept) => {
            let names: Vec<String> = kept.iter().map(|(name, _)| name.clone()).collect();
            if !names.is_empty() {
                let _: () = redis::cmd("UNLINK")
                    .arg(&names)
                    .query_async(&mut *conn)
                    .await?;
            }
            for (key, value) in kept {
                match value {
                    Value::String(bytes) => {
                        let _: () = redis::cmd("SET")
                            .arg(&key)
                            .arg(bytes)
                            .query_async(&mut *conn)
                            .await?;
                    }
                    Value::List(entries) => {
                        for slice in entries.chunks(LIST_BATCH as usize) {
                            let _: () = redis::cmd("RPUSH")
                                .arg(&key)
                                .arg(slice)
                                .query_async(&mut *conn)
                                .await?;
                        }
                    }
                    Value::Hash(pairs) => {
                        for slice in pairs.chunks(LIST_BATCH as usize) {
                            let mut command = redis::cmd("HSET");
                            command.arg(&key);
                            for (field, value) in slice {
                                command.arg(field).arg(value);
                            }
                            let _: () = command.query_async(&mut *conn).await?;
                        }
                    }
                }
                let _: () = redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(ttl)
                    .query_async(&mut *conn)
                    .await?;
            }
        }
        // The first layout's, kept for a room paused under it.
        Stored::Dumps(dumps) => {
            for (key, dump) in dumps {
                let _: () = redis::cmd("RESTORE")
                    .arg(&key)
                    .arg(HISTORY_TTL * 1000)
                    .arg(dump)
                    .arg("REPLACE")
                    .query_async(&mut *conn)
                    .await?;
            }
        }
    }
    drop(conn);
    state
        .redis_state
        .update_room_activity(room_uuid)
        .await
        .map_err(|e| -> Error { format!("{e}").into() })?;
    Ok(())
}

/// Puts a paused room back before somebody joins it; nothing to do for one
/// that is not paused. Waits behind a pause or a resume already under way.
pub async fn resume_if_paused(state: &AppState, room_uuid: Uuid) -> Result<(), Error> {
    let mut tx = state.db_pool.begin().await?;
    if !lock_paused_room(&mut *tx, room_uuid).await? {
        return Ok(());
    }
    let stored = get_object(state, &object_key(room_uuid)).await?;
    restore(state, room_uuid, decode(&stored)?).await?;
    let preview_version = sqlx::query_scalar!(
        "SELECT paused_preview_version FROM collaborative_sessions WHERE id = $1",
        room_uuid
    )
    .fetch_one(&mut *tx)
    .await?;
    // The lobby's picture comes back with the room, rather than leaving its
    // card blank until somebody draws and a browser uploads a new one.
    if let Some(version) = preview_version {
        match stored_preview(state, room_uuid, version as u64).await {
            Ok(preview) => put_back_preview(state, room_uuid, &preview).await,
            Err(e) => warn!("Failed to bring back room {room_uuid}'s preview: {e}"),
        }
    }
    // Fresh activity, or the next sweep would find it idle and pause it
    // again under the person who just came back.
    sqlx::query!(
        r#"
        UPDATE collaborative_sessions
        SET paused_at = NULL, paused_preview_version = NULL, last_activity = NOW()
        WHERE id = $1
        "#,
        room_uuid
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut stored_keys = vec![object_key(room_uuid)];
    if preview_version.is_some() {
        stored_keys.push(preview_object_key(room_uuid));
    }
    for key in stored_keys {
        if let Err(e) = delete_object(state, &key).await {
            // Harmless: the room is back, and the next pause overwrites it.
            warn!("Failed to remove room {room_uuid}'s stored {key}: {e}");
        }
    }
    info!("Resumed room {room_uuid}");
    Ok(())
}

/// Copies a pausing room's lobby preview into the bucket, so its card goes on
/// showing which drawing it is. None when it has none, or it could not be
/// kept: that costs the card its picture, never the pause.
async fn keep_preview(state: &AppState, room_uuid: Uuid) -> Option<StoredPreview> {
    let preview = match PreviewStore::new(state.redis_pool.clone())
        .load(room_uuid)
        .await
    {
        Ok(preview) => preview?,
        Err(e) => {
            warn!("Failed to read pausing room {room_uuid}'s preview: {e}");
            return None;
        }
    };
    match put_object(state, &preview_object_key(room_uuid), preview.bytes.clone()).await {
        Ok(()) => Some(preview),
        Err(e) => {
            warn!("Failed to keep pausing room {room_uuid}'s preview: {e}");
            None
        }
    }
}

async fn put_back_preview(state: &AppState, room_uuid: Uuid, preview: &StoredPreview) {
    if let Err(e) = PreviewStore::new(state.redis_pool.clone())
        .put(room_uuid, preview)
        .await
    {
        warn!("Failed to put back room {room_uuid}'s preview: {e}");
    }
}

/// A kept preview, read back. Its format is read off the bytes, which were
/// checked by `inspect_image` when they were first uploaded.
async fn stored_preview(
    state: &AppState,
    room_uuid: Uuid,
    version: u64,
) -> Result<StoredPreview, Error> {
    let bytes = get_object(state, &preview_object_key(room_uuid)).await?;
    let (kind, _, _) = inspect_image(&bytes).ok_or("a kept preview that is not an image")?;
    Ok(StoredPreview {
        bytes,
        kind,
        version,
    })
}

/// The preview versions that those of these rooms which are paused took with
/// them, by room. A room absent had none, or is not paused.
pub async fn paused_preview_versions(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuids: &[Uuid],
) -> Result<HashMap<Uuid, u64>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT id, paused_preview_version AS "version!"
        FROM collaborative_sessions
        WHERE id = ANY($1)
          AND paused_at IS NOT NULL
          AND ended_at IS NULL
          AND paused_preview_version IS NOT NULL
        "#,
        room_uuids
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.id, row.version as u64))
        .collect())
}

/// A paused room's lobby preview, from the bucket. None when the room is not
/// paused or took no preview with it.
pub async fn paused_preview(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Option<StoredPreview>, Error> {
    let Some(version) = paused_preview_versions(&state.db_pool, &[room_uuid])
        .await?
        .remove(&room_uuid)
    else {
        return Ok(None);
    };
    Ok(Some(stored_preview(state, room_uuid, version).await?))
}

/// Ends the rooms that have been paused longer than [`PAUSED_RETENTION`],
/// and lets their stored state go. Returns how many.
pub async fn end_abandoned(state: &AppState) -> Result<usize, Error> {
    let ended = sqlx::query_scalar!(
        r#"
        UPDATE collaborative_sessions SET ended_at = NOW()
        WHERE paused_at IS NOT NULL AND ended_at IS NULL
          AND paused_at < NOW() - make_interval(secs => $1)
        RETURNING id
        "#,
        PAUSED_RETENTION.as_secs_f64()
    )
    .fetch_all(&state.db_pool)
    .await?;
    for room_uuid in &ended {
        for key in [object_key(*room_uuid), preview_object_key(*room_uuid)] {
            if let Err(e) = delete_object(state, &key).await {
                warn!("Failed to remove abandoned room {room_uuid}'s stored {key}: {e}");
            }
        }
        info!("Ended room {room_uuid}: paused for longer than its retention");
        crate::staff_webhook::notify(crate::staff_webhook::Event::RoomAbandoned {
            room: *room_uuid,
        });
    }
    Ok(ended.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_kept(stored: Stored) -> Kept {
        match stored {
            Stored::Kept(kept) => kept,
            Stored::Dumps(_) => panic!("read as the first layout"),
        }
    }

    #[test]
    fn a_stored_pause_reads_back_as_written() {
        let kept: Kept = vec![
            ("oeee:seq".to_string(), Value::String(b"42".to_vec())),
            (
                "oeee:history".to_string(),
                Value::List(vec![vec![1, 2, 3], vec![], vec![0; 70_000]]),
            ),
            (
                "오이:seats".to_string(),
                Value::Hash(vec![
                    (b"__next".to_vec(), b"3".to_vec()),
                    (b"a".to_vec(), b"1".to_vec()),
                ]),
            ),
            ("oeee:empty".to_string(), Value::List(vec![])),
        ];
        assert_eq!(stored_kept(decode(&encode(&kept).unwrap()).unwrap()), kept);
    }

    #[test]
    fn the_first_layout_is_still_read() {
        let mut raw = Vec::from(MAGIC_DUMPS);
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.push(b'k');
        raw.extend_from_slice(&3u32.to_le_bytes());
        raw.extend_from_slice(&[7, 8, 9]);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        match decode(&encoder.finish().unwrap()).unwrap() {
            Stored::Dumps(dumps) => assert_eq!(dumps, vec![("k".to_string(), vec![7, 8, 9])]),
            Stored::Kept(_) => panic!("read as the second layout"),
        }
    }

    #[test]
    fn something_that_is_not_a_pause_is_refused() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"OEEELOG\x02 nothing like it").unwrap();
        assert!(decode(&encoder.finish().unwrap()).is_err());
        let whole = encode(&vec![("k".to_string(), Value::List(vec![vec![9; 40]]))]).unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(&whole[..]).read_to_end(&mut raw).unwrap();
        raw.truncate(raw.len() - 5);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        assert!(decode(&encoder.finish().unwrap()).is_err(), "cut short");
    }
}
