//! What is recorded in Redis before it is written out: the messages, and the
//! room's chat.

use redis::AsyncCommands;
use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use crate::redis::RedisPool;
use crate::web::state::AppState;

use super::{
    archive_claim_key, bucket, buffer_key, chat_buffer_key, decode_buffered, ArchivedMessage,
    BufferResult, ARCHIVE_BUFFER_TTL, FLUSH_CLAIM_TTL, MAX_CHAT_LINES,
};

/// The un-flushed tail of a room's recording.
#[derive(Clone)]
pub struct ArchiveBuffer {
    pool: RedisPool,
}

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
        Ok(raw
            .iter()
            .filter_map(|entry| decode_buffered(entry))
            .collect())
    }

    /// Everything waiting, for a reader that wants the recording as it
    /// stands without writing anything out.
    pub async fn peek_all(&self, room_uuid: Uuid) -> BufferResult<Vec<ArchivedMessage>> {
        let mut conn = self.pool.get().await?;
        let raw: Vec<Vec<u8>> = conn.lrange(buffer_key(room_uuid), 0, -1).await?;
        Ok(raw
            .iter()
            .filter_map(|entry| decode_buffered(entry))
            .collect())
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
    let Some(chat) = crate::web::handlers::collaborate::messages::ChatMessage::parse(frame) else {
        warn!(
            "Could not read a chat frame for room {} to record it",
            room_uuid
        );
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
        conn.expire::<_, ()>(&key, ARCHIVE_BUFFER_TTL as i64)
            .await?;
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
pub(super) async fn buffered_chat(
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

pub(super) async fn clear_chat_buffer(state: &AppState, room_uuid: Uuid) -> BufferResult<()> {
    let mut conn = state.redis_pool.get().await?;
    conn.del::<_, ()>(chat_buffer_key(room_uuid)).await?;
    Ok(())
}
