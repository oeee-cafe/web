//! Pausing a room nobody is drawing in, rather than ending it.
//!
//! A room's drawing lives in Redis, and Redis lets it go an hour after the
//! last connection: a room that ran out of activity before its owner saved it
//! used to lose the drawing for good, with only the owner able to have saved
//! it. Now an idle room with an unsaved drawing is *paused*: its keys are
//! taken out of Redis whole (`DUMP`), stored as one object in the private
//! bucket, and the session stays open. The first person allowed in puts them
//! back (`RESTORE`) before they are sent the history, and the room carries on
//! from exactly where it stood -- the same history id, so a page that was open
//! when it paused resumes rather than replays.
//!
//! A paused room ends for good after [`PAUSED_RETENTION`]. A room with nothing
//! drawn in it is not worth keeping and ends as it always did.
//!
//! ## Ordering
//!
//! Pausing and resuming both hold the session row `FOR UPDATE`, which is what
//! every join takes too (`track_participant_with_capacity_check`). So nobody
//! joins a room halfway through being paused, and the first to arrive at a
//! paused one puts it back while the others wait their turn behind the lock
//! and find it already back. The Redis keys are deleted before the pause
//! commits: deleted after, a join waiting on the lock would restore them and
//! then watch them be deleted.
//!
//! `DUMP` is in Redis's own serialisation, which a Redis can always read from
//! itself or an older version; a paused room would not survive a downgrade of
//! Redis inside its retention, which nobody does.

use std::io::{Read, Write};
use std::time::Duration;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::redis_messages::{self, HISTORY_TTL};
use super::websocket::{send_away_everyone, Departure};
use crate::web::state::AppState;

/// How long a paused room waits for somebody to come back to it.
pub const PAUSED_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What a stored pause begins with, and its layout version.
const MAGIC: &[u8] = b"OEEEPAUSE\x01";

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Where a paused room's state is kept in the bucket.
fn object_key(room_uuid: Uuid) -> String {
    format!("paused/{room_uuid}")
}

/// The keys a pause carries: the room's drawing, and its recording's chat
/// buffer, whose lifetime is a day and so shorter than a pause.
fn carried_keys(room_uuid: Uuid) -> Vec<String> {
    let mut keys = redis_messages::room_keys(room_uuid);
    keys.push(super::archive::chat_buffer_key(room_uuid));
    keys
}

/// `[magic][count:2]` then each key `[name len:2][name][dump len:4][dump]`,
/// the whole compressed. Keys that do not exist are not written.
fn encode(dumps: &[(String, Vec<u8>)]) -> Result<Vec<u8>, Error> {
    let mut raw = Vec::from(MAGIC);
    raw.extend_from_slice(&(dumps.len() as u16).to_le_bytes());
    for (name, dump) in dumps {
        raw.extend_from_slice(&(name.len() as u16).to_le_bytes());
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(&(dump.len() as u32).to_le_bytes());
        raw.extend_from_slice(dump);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

fn decode(stored: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let mut raw = Vec::new();
    ZlibDecoder::new(stored).read_to_end(&mut raw)?;
    let mut at = MAGIC.len();
    if !raw.starts_with(MAGIC) || raw.len() < at + 2 {
        return Err("not a paused room".into());
    }
    let take = |at: &mut usize, len: usize| -> Result<&[u8], Error> {
        let slice = raw.get(*at..*at + len).ok_or("a paused room cut short")?;
        *at += len;
        Ok(slice)
    };
    let count = u16::from_le_bytes(take(&mut at, 2)?.try_into()?) as usize;
    let mut dumps = Vec::with_capacity(count);
    for _ in 0..count {
        let name_len = u16::from_le_bytes(take(&mut at, 2)?.try_into()?) as usize;
        let name = String::from_utf8(take(&mut at, name_len)?.to_vec())?;
        let dump_len = u32::from_le_bytes(take(&mut at, 4)?.try_into()?) as usize;
        dumps.push((name, take(&mut at, dump_len)?.to_vec()));
    }
    Ok(dumps)
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
    let open = sqlx::query_scalar!(
        r#"
        SELECT id FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL AND paused_at IS NULL
        FOR UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(&mut *tx)
    .await?;
    if open.is_none() {
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
    let mut conn = state.redis_pool.get().await?;
    let mut dumps = Vec::new();
    for key in &keys {
        let dump: Option<Vec<u8>> = redis::cmd("DUMP").arg(key).query_async(&mut *conn).await?;
        if let Some(dump) = dump {
            dumps.push((key.clone(), dump));
        }
    }
    let body = encode(&dumps)?;
    let size = body.len();
    put_object(state, &object_key(room_uuid), body).await?;

    let _: () = redis::cmd("DEL").arg(&keys).query_async(&mut *conn).await?;
    drop(conn);
    for cleaned in [
        state.redis_state.cleanup_room_state(room_uuid).await,
        super::preview::PreviewStore::new(state.redis_pool.clone())
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
            "UPDATE collaborative_sessions SET paused_at = NOW() WHERE id = $1",
            room_uuid
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
            if let Err(restore) = restore(state, room_uuid, &dumps).await {
                error!(
                    "Room {room_uuid} could not be marked paused ({e}) nor put back ({restore}); its drawing is in the bucket at {}",
                    object_key(room_uuid)
                );
            }
            return Err(e.into());
        }
    }
    info!(
        "Paused room {room_uuid}: {} keys, {size} bytes",
        dumps.len()
    );
    Ok(Idled::Paused)
}

/// Puts the keys back, alive for as long as a room's history is.
async fn restore(
    state: &AppState,
    room_uuid: Uuid,
    dumps: &[(String, Vec<u8>)],
) -> Result<(), Error> {
    let mut conn = state.redis_pool.get().await?;
    for (key, dump) in dumps {
        let _: () = redis::cmd("RESTORE")
            .arg(key)
            .arg(HISTORY_TTL * 1000)
            .arg(dump)
            .arg("REPLACE")
            .query_async(&mut *conn)
            .await?;
    }
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
    let paused = sqlx::query_scalar!(
        r#"
        SELECT paused_at IS NOT NULL AS "paused!" FROM collaborative_sessions
        WHERE id = $1 AND ended_at IS NULL
        FOR UPDATE
        "#,
        room_uuid
    )
    .fetch_optional(&mut *tx)
    .await?;
    if paused != Some(true) {
        return Ok(());
    }
    let stored = get_object(state, &object_key(room_uuid)).await?;
    restore(state, room_uuid, &decode(&stored)?).await?;
    // Fresh activity, or the next sweep would find it idle and pause it
    // again under the person who just came back.
    sqlx::query!(
        "UPDATE collaborative_sessions SET paused_at = NULL, last_activity = NOW() WHERE id = $1",
        room_uuid
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Err(e) = delete_object(state, &object_key(room_uuid)).await {
        // Harmless: the room is back, and the next pause overwrites it.
        warn!("Failed to remove room {room_uuid}'s stored pause: {e}");
    }
    info!("Resumed room {room_uuid}");
    Ok(())
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
        if let Err(e) = delete_object(state, &object_key(*room_uuid)).await {
            warn!("Failed to remove abandoned room {room_uuid}'s stored pause: {e}");
        }
        info!("Ended room {room_uuid}: paused for longer than its retention");
    }
    Ok(ended.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_pause_reads_back_as_written() {
        let dumps = vec![
            ("oeee:a".to_string(), vec![1, 2, 3]),
            ("oeee:b".to_string(), vec![]),
            ("오이".to_string(), vec![0; 70_000]),
        ];
        assert_eq!(decode(&encode(&dumps).unwrap()).unwrap(), dumps);
    }

    #[test]
    fn something_that_is_not_a_pause_is_refused() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"OEEELOG\x02 nothing like it").unwrap();
        assert!(decode(&encoder.finish().unwrap()).is_err());
        let whole = encode(&[("k".to_string(), vec![9; 40])]).unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(&whole[..]).read_to_end(&mut raw).unwrap();
        raw.truncate(raw.len() - 5);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        assert!(decode(&encoder.finish().unwrap()).is_err(), "cut short");
    }
}
