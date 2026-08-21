use super::redis_messages;
use crate::web::state::AppState;
use axum::extract::ws::Message;
use sqlx::{Pool, Postgres};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::db;
use super::utils::{bytes_to_uuid, read_u64_le};

#[cfg(test)]
mod chat_framing_tests {
    use super::ChatMessage;
    use uuid::Uuid;

    /// A frame the server wrote has to read back as what it wrote, including
    /// the awkward text people actually send.
    #[test]
    fn a_chat_message_round_trips() {
        for message in [
            "hello",
            "",
            "여기까지 하죠",
            "emoji 🎨 and a | pipe and a : colon",
            "a\nnewline",
        ] {
            let original = ChatMessage {
                user_id: Uuid::from_u128(9),
                timestamp: 1_700_000_000_123,
                username: "miro".to_string(),
                message: message.to_string(),
            };
            assert_eq!(
                ChatMessage::parse(&original.serialize()).map(|held| held.message),
                Some(message.to_string()),
                "{message:?}"
            );
        }
    }

    #[test]
    fn rejects_a_frame_that_is_not_chat_or_is_cut_short() {
        let whole = ChatMessage {
            user_id: Uuid::from_u128(1),
            timestamp: 1,
            username: "umu".to_string(),
            message: "hi".to_string(),
        }
        .serialize();
        assert!(ChatMessage::parse(&whole).is_some());
        for length in 0..whole.len() {
            assert!(ChatMessage::parse(&whole[..length]).is_none(), "{length}");
        }
        let mut wrong_type = whole.clone();
        wrong_type[0] = 0x16;
        assert!(ChatMessage::parse(&wrong_type).is_none());
    }
}

pub struct EndSessionContext<'a> {
    pub user_id: Uuid,
    pub user_login_name: &'a str,
    pub room_uuid: Uuid,
    pub is_owner: bool,
    pub db: &'a Pool<Postgres>,
    pub state: &'a AppState,
    pub msg: &'a Message,
    pub connection_id: &'a str,
}

// Safe timestamp helper to avoid panics from system clock issues
fn get_current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_millis() as u64
}

// Message type constants matching neo-cucumber protocol
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Join = 0x01,
    Snapshot = 0x02,
    Chat = 0x03,
    // Answers a reset query: this client is caught up and willing to upload
    // the checkpoint (client -> server).
    //
    // Only ever sent in reply to a query, which only a server that knows this
    // message sends -- so a client cannot put one in front of a server from
    // before it existed, where an unknown type would be sequenced into history
    // and leave every client in the room reading a message it cannot apply.
    // That is why the phase rides on RESET_REQUEST instead of being a message
    // of its own: it keeps this one unreachable from an older server.
    ResetOffer = 0x04,
    ReplayStart = 0x05,
    Layers = 0x06,
    EndSession = 0x07,
    SessionExpired = 0x08,
    Leave = 0x09,
    // Wraps a history message with its canonical sequence number (server -> client)
    Sequenced = 0x0A,
    // Asks one chosen client to upload a session reset (server -> client)
    ResetRequest = 0x0B,
    // Announces a session reset upload: base seq + snapshot count (client -> server)
    ResetBegin = 0x0C,
    // Notifies clients that history at or below a base seq was squashed (server -> client)
    ResetPoint = 0x0D,
    // Tells a connecting client its 1-byte session user id (server -> client)
    Welcome = 0x0E,
    // Declares the exact canonical position reached by initial replay.
    CaughtUp = 0x0F,
}

// Message structures
#[derive(Debug, Clone)]
pub struct JoinMessage {
    pub user_id: Uuid,
    pub timestamp: u64,
    pub username: String,
}

#[derive(Debug, Clone)]
pub struct LayersMessage {
    // (user_uuid, session_id, login_name, join_timestamp)
    pub participants: Vec<(Uuid, u8, String, i64)>,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub user_id: Uuid,
    pub timestamp: u64,
    pub username: String,
    pub message: String,
}

/// The two things the server can say to a client about a checkpoint.
///
/// Drawpile asks before it tells: a reset query goes to every eligible client,
/// and whoever answers first is the one asked to do the work
/// (`ThinSession::checkAutoResetQuery`). Making the checkpoint is expensive --
/// a PNG of every layer of every participant -- and a client that is mid
/// catch-up cannot make a correct one at all, so asking a client picked by the
/// server from what little it knows about them means the request can land on
/// the one client unable to answer it, and the room waits out the whole
/// pending window for a checkpoint that was never coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResetPhase {
    /// "Can anyone here upload a checkpoint?" -- sent to the whole room.
    Query = 0,
    /// "You have it, go ahead." -- sent to the one connection that answered.
    Upload = 1,
}

#[derive(Debug, Clone)]
pub struct ResetRequestMessage {
    pub timestamp: u64,
    pub phase: ResetPhase,
}

#[derive(Debug, Clone)]
pub struct LeaveMessage {
    pub user_id: Uuid,
    pub timestamp: u64,
    pub username: String,
}

// Message serialization functions
impl JoinMessage {
    pub fn serialize(&self) -> Vec<u8> {
        let username_bytes = self.username.as_bytes();
        let username_len = username_bytes.len() as u16;

        let mut buffer = Vec::with_capacity(1 + 16 + 8 + 2 + username_bytes.len());
        buffer.push(MessageType::Join as u8);
        buffer.extend_from_slice(self.user_id.as_bytes());
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        buffer.extend_from_slice(&username_len.to_le_bytes());
        buffer.extend_from_slice(username_bytes);

        buffer
    }
}

impl LayersMessage {
    pub fn serialize(&self) -> Vec<u8> {
        let participant_count = self.participants.len() as u16;

        // 1 + 2 + per participant: uuid(16) + session id(1) + name len(2) + name + timestamp(8)
        let total_size = 1
            + 2
            + self
                .participants
                .iter()
                .map(|(_, _, name, _)| 16 + 1 + 2 + name.len() + 8)
                .sum::<usize>();

        let mut buffer = Vec::with_capacity(total_size);

        buffer.push(MessageType::Layers as u8);
        buffer.extend_from_slice(&participant_count.to_le_bytes());

        for (user_uuid, session_id, login_name, join_timestamp) in &self.participants {
            // Add user UUID (16 bytes) and 1-byte session id
            buffer.extend_from_slice(user_uuid.as_bytes());
            buffer.push(*session_id);

            // Add login name length (2 bytes) + login name
            let name_bytes = login_name.as_bytes();
            let name_len = name_bytes.len() as u16;
            buffer.extend_from_slice(&name_len.to_le_bytes());
            buffer.extend_from_slice(name_bytes);

            // Add join timestamp (8 bytes)
            buffer.extend_from_slice(&join_timestamp.to_le_bytes());
        }

        buffer
    }
}

impl ChatMessage {
    /// The inverse of `serialize`, for anything that has to read a chat frame
    /// back rather than pass it along.
    ///
    /// Here beside the writer because the layout is one thing: a second reader
    /// somewhere else is how the two drift, and the frame this parses is the
    /// one the server built, with the name it authenticated rather than the
    /// one the client claimed.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 27 || data[0] != MessageType::Chat as u8 {
            return None;
        }
        let user_id = bytes_to_uuid(&data[1..17]).ok()?;
        let timestamp = read_u64_le(data, 17);
        let username_len = u16::from_le_bytes([data[25], data[26]]) as usize;
        let username_end = 27 + username_len;
        let username = std::str::from_utf8(data.get(27..username_end)?).ok()?;
        let message_len =
            u16::from_le_bytes([*data.get(username_end)?, *data.get(username_end + 1)?]) as usize;
        let message_start = username_end + 2;
        let message = std::str::from_utf8(data.get(message_start..message_start + message_len)?)
            .ok()?;
        Some(Self {
            user_id,
            timestamp,
            username: username.to_string(),
            message: message.to_string(),
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let username_bytes = self.username.as_bytes();
        let username_len = username_bytes.len() as u16;
        let message_bytes = self.message.as_bytes();
        let message_len = message_bytes.len() as u16;

        let mut buffer =
            Vec::with_capacity(1 + 16 + 8 + 2 + username_bytes.len() + 2 + message_bytes.len());
        buffer.push(MessageType::Chat as u8);
        buffer.extend_from_slice(self.user_id.as_bytes());
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        buffer.extend_from_slice(&username_len.to_le_bytes());
        buffer.extend_from_slice(username_bytes);
        buffer.extend_from_slice(&message_len.to_le_bytes());
        buffer.extend_from_slice(message_bytes);

        buffer
    }
}

impl ResetRequestMessage {
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(1 + 8 + 1);
        buffer.push(MessageType::ResetRequest as u8);
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        // The phase is appended rather than given its own message type so that
        // a client from before the query phase existed still understands the
        // frame -- it reads the timestamp, ignores the byte after it, and
        // starts uploading. That client answers a query by doing the work; if
        // it wins the claim the checkpoint is real, and if it loses,
        // `parse_reset_begin` counts its snapshots off the wire and drops them.
        buffer.push(self.phase as u8);

        buffer
    }
}

impl LeaveMessage {
    pub fn serialize(&self) -> Vec<u8> {
        let username_bytes = self.username.as_bytes();
        let username_len = username_bytes.len() as u16;

        let mut buffer = Vec::with_capacity(1 + 16 + 8 + 2 + username_bytes.len());
        buffer.push(MessageType::Leave as u8);
        buffer.extend_from_slice(self.user_id.as_bytes());
        buffer.extend_from_slice(&self.timestamp.to_le_bytes());
        buffer.extend_from_slice(&username_len.to_le_bytes());
        buffer.extend_from_slice(username_bytes);

        buffer
    }
}

// Message parsing utilities
pub fn parse_message_type(data: &[u8]) -> Option<MessageType> {
    if data.is_empty() {
        return None;
    }

    match data[0] {
        0x01 => Some(MessageType::Join),
        0x02 => Some(MessageType::Snapshot),
        0x03 => Some(MessageType::Chat),
        0x04 => Some(MessageType::ResetOffer),
        0x05 => Some(MessageType::ReplayStart),
        0x06 => Some(MessageType::Layers),
        0x07 => Some(MessageType::EndSession),
        0x08 => Some(MessageType::SessionExpired),
        0x09 => Some(MessageType::Leave),
        0x0A => Some(MessageType::Sequenced),
        0x0B => Some(MessageType::ResetRequest),
        0x0C => Some(MessageType::ResetBegin),
        0x0D => Some(MessageType::ResetPoint),
        0x0E => Some(MessageType::Welcome),
        0x0F => Some(MessageType::CaughtUp),
        _ => None,
    }
}

pub fn is_server_message(msg_type: u8) -> bool {
    msg_type < 0x10
}

pub fn is_client_message(msg_type: u8) -> bool {
    msg_type >= 0x10
}

// bytes_to_uuid is already available from utils module via import

pub async fn handle_join_message(
    data: &[u8],
    user_id: Uuid,
    user_login_name: &str,
    room_uuid: Uuid,
    db: &Pool<Postgres>,
    state: &AppState,
) -> Option<Message> {
    if data.len() < 25 {
        return None;
    }

    let user_uuid = match bytes_to_uuid(&data[1..17]) {
        Ok(uuid) => uuid,
        Err(_) => return None,
    };

    let timestamp = read_u64_le(data, 17);

    if user_uuid != user_id {
        return None;
    }

    debug!(
        "JOIN message from user {} ({}) at {} in room {}",
        user_login_name, user_uuid, timestamp, room_uuid
    );

    let join_message = JoinMessage {
        user_id,
        timestamp,
        username: user_login_name.to_string(),
    };

    if let Err(e) = db::track_join_participant(db, room_uuid, user_uuid, timestamp as i64).await {
        error!("Failed to track JOIN participant: {}", e);
    } else {
        broadcast_layers(db, room_uuid, state, user_id).await;
    }

    Some(Message::Binary(join_message.serialize()))
}

async fn get_session_participants(
    session_id: Uuid,
    state: &AppState,
) -> Result<Vec<(Uuid, String, i64)>, Box<dyn std::error::Error + Send + Sync>> {
    let db = &state.db_pool;

    // Query participants from the database, ordered by join time
    let rows = sqlx::query!(
        r#"
        SELECT csp.user_id, u.login_name, csp.joined_at
        FROM collaborative_sessions_participants csp
        JOIN users u ON csp.user_id = u.id
        WHERE csp.session_id = $1 AND csp.is_active = true
        ORDER BY csp.joined_at ASC
        "#,
        session_id
    )
    .fetch_all(db)
    .await?;

    let participants: Vec<(Uuid, String, i64)> = rows
        .into_iter()
        .map(|row| {
            // Convert timestamp to milliseconds since epoch
            let millis = row.joined_at.and_utc().timestamp_millis();
            (row.user_id, row.login_name, millis)
        })
        .collect();

    Ok(participants)
}

/// Who is in the room and which 1-byte id each of them draws under.
///
/// Presence travels beside the canonical drawing stream rather than inside it,
/// which means a stroke can reach a client before the message naming its
/// author. That costs a name on a cursor for a moment and nothing else: a
/// participant's place in the canvas stack comes from their session id, not
/// from when this message happened to arrive (`canvasStack.inJoinOrder`), and
/// their layers are allocated on first mention. Drawpile puts presence in
/// history because its history is the transport for everything; here it would
/// buy ordering for something that does not need it, at the cost of replaying
/// joins and leaves to everyone who ever catches up.
///
/// What does matter is that a *connecting* client has the mapping before the
/// history it is about to replay -- see `handle_socket`.
pub async fn current_layers_message(room_uuid: Uuid, state: &AppState) -> Option<Vec<u8>> {
    // Use database as the canonical source for participant join order
    let participants = match get_session_participants(room_uuid, state).await {
        Ok(participants) => participants,
        Err(e) => {
            error!("Failed to query participants for room {}: {}", room_uuid, e);
            return None;
        }
    };

    // Attach the 1-byte session ids from Redis (0 = unknown/expired)
    let session_ids = state
        .redis_state
        .get_user_ids(room_uuid)
        .await
        .unwrap_or_default();

    // Participants are already sorted by join time from the database query
    Some(
        LayersMessage {
            participants: participants
                .into_iter()
                .map(|(uuid, name, ts)| {
                    (uuid, session_ids.get(&uuid).copied().unwrap_or(0), name, ts)
                })
                .collect(),
        }
        .serialize(),
    )
}

async fn broadcast_layers(_db: &Pool<Postgres>, room_uuid: Uuid, state: &AppState, _user_id: Uuid) {
    let Some(payload) = current_layers_message(room_uuid, state).await else {
        return;
    };

    // Use Redis pub/sub to send LAYERS message to all connections in the room
    let room_message = super::redis_state::RoomBroadcast {
        from_connection: "system".to_string(),
        target_connection: None,
        seq: None,
        history_id: None,
        payload,
    };

    match state
        .redis_state
        .publish_message(room_uuid, &room_message)
        .await
    {
        Ok(subscriber_count) => {
            info!(
                "Broadcasted LAYERS (ordered by join time) to {} subscribers in room {}",
                subscriber_count, room_uuid
            );
        }
        Err(e) => {
            error!(
                "Failed to publish LAYERS message for room {}: {}",
                room_uuid, e
            );
        }
    }
}

// Note: send_existing_participants_to_new_user function is not needed
// JOIN and LEAVE messages are ephemeral (not stored in history)
// Current participants are communicated via JOIN_RESPONSE messages

pub fn handle_chat_message(data: &[u8], user_id: Uuid, user_login_name: &str) -> Option<Message> {
    if data.len() < 27 {
        return None;
    }

    let chat_user = match bytes_to_uuid(&data[1..17]) {
        Ok(user) => user,
        Err(_) => return None,
    };

    if chat_user != user_id {
        return None;
    }

    let timestamp = read_u64_le(data, 17);
    let msg_length = u16::from_le_bytes([data[25], data[26]]) as usize;

    if data.len() < 27 + msg_length {
        return None;
    }

    let chat_text = match std::str::from_utf8(&data[27..27 + msg_length]) {
        Ok(text) => text,
        Err(_) => return None,
    };

    debug!(
        "Chat message from user {} ({}): {}",
        user_login_name, chat_user, chat_text
    );

    let chat_message = ChatMessage {
        user_id,
        timestamp,
        username: user_login_name.to_string(),
        message: chat_text.to_string(),
    };

    Some(Message::Binary(chat_message.serialize()))
}

pub async fn handle_end_session_message(data: &[u8], ctx: EndSessionContext<'_>) {
    if ctx.is_owner && data.len() >= 19 {
        if let Ok(sender_uuid) = bytes_to_uuid(&data[1..17]) {
            if sender_uuid == ctx.user_id {
                let url_length = u16::from_le_bytes([data[17], data[18]]) as usize;

                if data.len() >= 19 + url_length {
                    if let Ok(post_url) = std::str::from_utf8(&data[19..19 + url_length]) {
                        info!(
                            "END_SESSION from owner {} in session {}, redirecting to: {}",
                            ctx.user_login_name, ctx.room_uuid, post_url
                        );

                        // Set collaborative_sessions.ended_at
                        if let Err(e) = db::end_session(ctx.db, ctx.room_uuid).await {
                            // END_SESSION is the client's acknowledgement. Do
                            // not publish it unless the authoritative database
                            // transition succeeded, or every participant would
                            // leave a session that is still open server-side.
                            error!("Failed to update session ended_at: {}", e);
                            return;
                        }

                        // Before the cleanup below: sealing reads the
                        // participant map, and that map is one of the keys
                        // that goes.
                        super::archive::seal_room(ctx.state, ctx.room_uuid, true).await;

                        // Clean up Redis message history when session is explicitly ended
                        let redis_store =
                            redis_messages::RedisMessageStore::new(ctx.state.redis_pool.clone());
                        if let Err(e) = redis_store.cleanup_room(ctx.room_uuid).await {
                            error!(
                                "Failed to cleanup Redis for ended session {}: {}",
                                ctx.room_uuid, e
                            );
                        } else {
                            info!(
                                "Cleaned up Redis message history for ended session {}",
                                ctx.room_uuid
                            );
                        }

                        // And the lobby preview, which is about a canvas that
                        // is now a post.
                        let preview_store =
                            super::preview::PreviewStore::new(ctx.state.redis_pool.clone());
                        if let Err(e) = preview_store.cleanup(ctx.room_uuid).await {
                            error!(
                                "Failed to cleanup the preview for ended session {}: {}",
                                ctx.room_uuid, e
                            );
                        }

                        // Broadcast END_SESSION to all participants in the room (including sender) via Redis pub/sub
                        let room_message = super::redis_state::RoomBroadcast {
                            from_connection: ctx.connection_id.to_string(),
                            target_connection: None,
                            seq: None,
                            history_id: None,
                            payload: ctx.msg.clone().into_data(),
                        };

                        match ctx
                            .state
                            .redis_state
                            .publish_message(ctx.room_uuid, &room_message)
                            .await
                        {
                            Ok(subscriber_count) => {
                                info!(
                                    "Broadcasted END_SESSION to {} subscribers in room {}",
                                    subscriber_count, ctx.room_uuid
                                );
                            }
                            Err(e) => {
                                error!(
                                    "Failed to publish END_SESSION message for room {}: {}",
                                    ctx.room_uuid, e
                                );
                            }
                        }
                    }
                }
            }
        }
    } else if !ctx.is_owner {
        warn!(
            "Non-owner {} attempted to end session {}",
            ctx.user_login_name, ctx.room_uuid
        );
    }
}

pub fn should_store_message(msg: &Message) -> bool {
    if let Message::Binary(data) = msg {
        if data.is_empty() {
            return false;
        }

        let msg_type = data[0];
        // Store all messages except ephemeral ones:
        // - Chat messages (ephemeral conversation)
        // - JOIN messages (current participants sent via JOIN_RESPONSE)
        // - LEAVE messages (current participants tracked in Redis presence)
        // - POINTER_UP and MOVE_POINTER (live cursor signals)
        msg_type != MessageType::Chat as u8
            && msg_type != MessageType::Join as u8
            && msg_type != MessageType::Leave as u8
            && msg_type != 0x13
            && msg_type != 0x1c
    } else {
        true
    }
}

/// Asks about a session reset: the whole room when `target_connection` is
/// None, one connection when it is Some.
///
/// One client provides the canvas state that replaces the accumulated history,
/// and which client that is settled by whoever answers the query first.
pub async fn send_reset_request(
    room_uuid: Uuid,
    target_connection: Option<&str>,
    phase: ResetPhase,
    state: &AppState,
) {
    let reset_request = ResetRequestMessage {
        timestamp: get_current_timestamp_ms(),
        phase,
    };

    let room_message = super::redis_state::RoomBroadcast {
        from_connection: "system".to_string(),
        target_connection: target_connection.map(str::to_string),
        seq: None,
        history_id: None,
        payload: reset_request.serialize(),
    };

    match state
        .redis_state
        .publish_message(room_uuid, &room_message)
        .await
    {
        Ok(subscribers) => match target_connection {
            Some(connection) => info!(
                "Asked connection {} in room {} to upload a checkpoint",
                connection, room_uuid
            ),
            None => info!(
                "Asked {} subscribers in room {} whether any can upload a checkpoint",
                subscribers, room_uuid
            ),
        },
        Err(e) => {
            error!(
                "Failed to publish reset request for room {}: {}",
                room_uuid, e
            );
        }
    }
}

fn to_room_broadcast(msg: &Message, connection_id: &str) -> super::redis_state::RoomBroadcast {
    super::redis_state::RoomBroadcast {
        from_connection: connection_id.to_string(),
        target_connection: None,
        seq: None,
        history_id: None,
        payload: match msg {
            Message::Binary(data) => data.clone(),
            Message::Text(text) => text.as_bytes().to_vec(),
            _ => vec![],
        },
    }
}

/// Broadcasts an ephemeral (non-history) message via plain Pub/Sub.
pub async fn broadcast_message(
    msg: &Message,
    room_uuid: Uuid,
    connection_id: &str,
    state: &AppState,
) {
    let room_message = to_room_broadcast(msg, connection_id);

    // Publish to Redis Pub/Sub - this will reach all server instances
    match state
        .redis_state
        .publish_message(room_uuid, &room_message)
        .await
    {
        Ok(subscriber_count) => {
            debug!(
                "Published message to Redis for room {} - {} subscribers",
                room_uuid, subscriber_count
            );
        }
        Err(e) => {
            error!(
                "Failed to publish message to Redis for room {}: {}",
                room_uuid, e
            );
        }
    }
}

/// Atomically appends a history message to the room's canonical sequence and
/// broadcasts it, so every client (including late joiners replaying history)
/// observes the same server-authoritative message order.
pub async fn sequence_and_broadcast(
    msg: &Message,
    room_uuid: Uuid,
    connection_id: &str,
    state: &AppState,
) -> Result<redis_messages::Sequenced, Box<dyn std::error::Error + Send + Sync>> {
    let payload = match msg {
        Message::Binary(data) => data.clone(),
        Message::Text(text) => text.as_bytes().to_vec(),
        _ => vec![],
    };

    let redis_store = redis_messages::RedisMessageStore::new(state.redis_pool.clone());
    let channel = state.redis_state.get_room_channel(room_uuid);
    let outcome = redis_store
        .sequence_and_publish_recording(
            room_uuid,
            &payload,
            connection_id,
            &channel,
            super::archive::bucket(&state.config).is_some(),
        )
        .await?;

    match outcome {
        redis_messages::Sequenced::Stored { seq, .. } => debug!(
            "Sequenced and broadcast message {} for room {}",
            seq, room_uuid
        ),
        redis_messages::Sequenced::HistoryFull { bytes } => error!(
            "Refused a message for room {}: history is at its {} byte ceiling ({} bytes)",
            room_uuid,
            redis_messages::MAX_HISTORY_BYTES,
            bytes
        ),
    }
    Ok(outcome)
}

// This function is now handled by Redis automatic limits and TTL
// The RedisMessageStore enforces limits during store_message operations

pub async fn send_leave_message(
    room_uuid: Uuid,
    connection_id: &str,
    user_id: Uuid,
    user_login_name: &str,
    state: &AppState,
) {
    // Check how many connections are in the room via Redis
    let redis_connections = match state.redis_state.get_room_connections(room_uuid).await {
        Ok(connections) => connections,
        Err(e) => {
            error!(
                "Failed to get Redis connections for leave message in room {}: {}",
                room_uuid, e
            );
            return;
        }
    };

    if redis_connections.len() <= 1 {
        debug!(
            "Not sending LEAVE message for user {} in room {} - only 1 or fewer connections",
            user_login_name, room_uuid
        );
        return;
    }

    let timestamp = get_current_timestamp_ms();

    let leave_message = LeaveMessage {
        user_id,
        timestamp,
        username: user_login_name.to_string(),
    };

    // Use Redis pub/sub to send LEAVE message to all connections except the one leaving
    let room_message = super::redis_state::RoomBroadcast {
        from_connection: connection_id.to_string(),
        target_connection: None,
        seq: None,
        history_id: None,
        payload: leave_message.serialize(),
    };

    match state
        .redis_state
        .publish_message(room_uuid, &room_message)
        .await
    {
        Ok(subscriber_count) => {
            info!(
                "Sent LEAVE notification for user {} to {} subscribers in room {}",
                user_login_name, subscriber_count, room_uuid
            );
        }
        Err(e) => {
            error!(
                "Failed to publish LEAVE message for user {} in room {}: {}",
                user_login_name, room_uuid, e
            );
        }
    }
}
