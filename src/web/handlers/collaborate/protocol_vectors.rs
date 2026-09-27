//! The server's half of `frontend/collaborate/test/protocolVectors.json`.
//!
//! Each side of the protocol was tested against itself: the Rust round trip
//! wrote a frame and read it back with Rust, and the client's tests decoded
//! frames its own test helpers built. A layout both sides got wrong the same
//! way, or one side changed without the other, passed both. This file holds
//! the server to the bytes in the shared vectors, which the client's
//! `protocolVectors.test.ts` decodes and encodes against the same file.

use serde_json::Value;
use uuid::Uuid;

use super::messages::{
    caught_up_frame, end_session_frame, handle_chat_message, replay_start_frame, reset_point_frame,
    welcome_frame, ChatMessage, JoinMessage, LayersMessage, LeaveMessage, ResetPhase,
    ResetRequestMessage,
};
use super::utils::bytes_to_uuid;
use super::websocket::{reset_snapshot_count, wrap_sequenced};

fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../../frontend/collaborate/test/protocolVectors.json"
    ))
    .expect("the vectors parse")
}

fn bytes(entry: &Value) -> Vec<u8> {
    data_encoding::HEXLOWER
        .decode(entry["hex"].as_str().expect("hex").as_bytes())
        .expect("hex decodes")
}

fn uuid(value: &Value) -> Uuid {
    Uuid::parse_str(value.as_str().expect("a uuid string")).expect("a uuid")
}

fn text(value: &Value) -> String {
    value.as_str().expect("a string").to_string()
}

fn number(value: &Value) -> u64 {
    value.as_u64().expect("a number")
}

/// What this server writes for the frame a vector describes, built from the
/// fields the client is expected to decode out of it.
fn server_frame(name: &str, d: &Value) -> Vec<u8> {
    match name {
        "welcome" => welcome_frame(number(&d["sessionId"]) as u8),
        "join" => JoinMessage {
            user_id: uuid(&d["userId"]),
            timestamp: number(&d["timestamp"]),
            username: text(&d["username"]),
        }
        .serialize(),
        "layers" => LayersMessage {
            participants: d["participants"]
                .as_array()
                .expect("participants")
                .iter()
                .map(|p| {
                    (
                        uuid(&p["userId"]),
                        number(&p["sessionId"]) as u8,
                        text(&p["username"]),
                        number(&p["joinTimestamp"]) as i64,
                    )
                })
                .collect(),
        }
        .serialize(),
        "chat" => ChatMessage {
            user_id: uuid(&d["userId"]),
            timestamp: number(&d["timestamp"]),
            username: text(&d["username"]),
            message: text(&d["message"]),
        }
        .serialize(),
        "leave" => LeaveMessage {
            user_id: uuid(&d["userId"]),
            timestamp: number(&d["timestamp"]),
            username: text(&d["username"]),
        }
        .serialize(),
        "resetRequestQuery" | "resetRequestUpload" => ResetRequestMessage {
            timestamp: number(&d["timestamp"]),
            phase: match d["phase"].as_str() {
                Some("query") => ResetPhase::Query,
                Some("upload") => ResetPhase::Upload,
                other => panic!("phase {other:?}"),
            },
        }
        .serialize(),
        "replayStart" => replay_start_frame(
            uuid(&d["historyId"]),
            number(&d["afterSeq"]),
            number(&d["lastSeq"]),
        ),
        "caughtUp" => caught_up_frame(uuid(&d["historyId"]), number(&d["lastSeq"])),
        "resetPoint" => {
            reset_point_frame(number(&d["baseSeq"]), number(&d["snapshotCount"]) as u16)
        }
        "endSession" => end_session_frame(uuid(&d["userId"]), &text(&d["postUrl"])),
        other => panic!("no server builder for the {other:?} vector: add one here"),
    }
}

#[test]
fn the_server_writes_the_frames_the_client_decodes() {
    let vectors = vectors();
    let server = vectors["server"].as_object().expect("server frames");
    assert!(server.len() >= 11, "only {} server vectors", server.len());
    for (name, entry) in server {
        assert_eq!(
            data_encoding::HEXLOWER.encode(&server_frame(name, &entry["decoded"])),
            entry["hex"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn a_sequenced_envelope_is_what_the_client_unwraps() {
    let vectors = vectors();
    let envelope = &vectors["sequenced"];
    let payload = bytes(&vectors["server"][envelope["payload"].as_str().unwrap()]);
    assert_eq!(
        wrap_sequenced(
            uuid(&envelope["historyId"]),
            number(&envelope["seq"]),
            &payload
        ),
        bytes(envelope)
    );
}

/// The client's CHAT carries no name — the server supplies the one it
/// authenticated — so what it sends and what the room hears are two layouts
/// under one type byte.
#[test]
fn the_server_reads_the_chat_the_client_sends() {
    let vectors = vectors();
    let chat = &vectors["client"]["chat"];
    let fields = &chat["fields"];
    let sender = uuid(&fields["userId"]);

    let relayed = handle_chat_message(&bytes(chat), sender, "oeee").expect("accepted");
    let axum::extract::ws::Message::Binary(relayed) = relayed else {
        panic!("a binary frame");
    };
    let relayed = ChatMessage::parse(&relayed).expect("the relayed chat parses");
    assert_eq!(relayed.user_id, sender);
    assert_eq!(relayed.timestamp, number(&fields["timestamp"]));
    assert_eq!(relayed.message, text(&fields["message"]));
    assert_eq!(relayed.username, "oeee");

    assert!(
        handle_chat_message(&bytes(chat), Uuid::from_u128(1), "someone").is_none(),
        "a chat claiming another sender is dropped"
    );
}

#[test]
fn the_server_reads_the_reset_begin_the_client_sends() {
    let vectors = vectors();
    let begin = &vectors["client"]["resetBegin"];
    assert_eq!(
        reset_snapshot_count(&bytes(begin)),
        Some((
            number(&begin["fields"]["lastSeq"]),
            number(&begin["fields"]["count"]) as u16
        ))
    );
}

/// The server reads only the sender off END_SESSION, to check it is the
/// owner's own; the URL it goes on to announce is the one it saved.
#[test]
fn the_server_reads_the_sender_of_the_end_session_the_client_sends() {
    let vectors = vectors();
    let end = &vectors["client"]["endSession"];
    let frame = bytes(end);
    assert!(frame.len() >= 19);
    assert_eq!(
        bytes_to_uuid(&frame[1..17]),
        Ok(uuid(&end["fields"]["userId"]))
    );
}
