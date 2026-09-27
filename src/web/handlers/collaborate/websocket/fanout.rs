//! Which of the room's broadcasts reach this connection, and the envelope
//! history travels in.

use uuid::Uuid;

use crate::web::handlers::collaborate::messages;

// Wraps a history message in
// [0x0A][history UUID: 16 bytes][seq: 8 bytes LE][payload], so clients only
// compare positions that belong to the same canonical history.
pub(in crate::web::handlers::collaborate) fn wrap_sequenced(
    history_id: Uuid,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(25 + payload.len());
    buf.push(messages::MessageType::Sequenced as u8);
    buf.extend_from_slice(history_id.as_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

pub(super) fn should_forward_to_connection(
    room_msg: &crate::web::handlers::collaborate::redis_state::RoomBroadcast,
    connection_id: &str,
) -> bool {
    // Targeted messages (e.g. RESET_REQUEST) go to exactly one connection
    if let Some(target) = &room_msg.target_connection {
        return target == connection_id;
    }
    if room_msg.from_connection != connection_id {
        return true;
    }
    // Echo every stored client drawing message back to its sender in canonical
    // server order so the client can confirm its optimistic fork. Keeping an
    // explicit list here lost newer operations (LINE was the first visible
    // casualty) whenever the protocol grew. Pointer messages are ephemeral
    // and must not enter reconciliation. Snapshot is a stored
    // server-range message; chat is echoed as delivery confirmation.
    // END_SESSION is the lifecycle transition the owner is waiting on: it is
    // published only once the session is really over, and the owner navigates
    // to the saved post on receiving it, so it must come back to its sender.
    match room_msg.payload.first().copied() {
        Some(0x02) | Some(0x03) | Some(0x07) => true,
        Some(msg_type) if messages::is_client_message(msg_type) => {
            msg_type != 0x13 && msg_type != 0x1c
        }
        _ => false,
    }
}
