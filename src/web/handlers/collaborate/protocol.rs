//! What a client is allowed to send, and what the server does not take its
//! word for.
//!
//! Two jobs, both borrowed from Drawpile's server:
//!
//! - **The author is the server's to decide.** Every canvas message carries a
//!   1-byte session user id in its second byte, and that id is what says whose
//!   layers a mark lands on and whose undo stack it joins. A client that writes
//!   somebody else's id there paints on their canvas and undoes their work.
//!   Drawpile does not validate this field, it *overwrites* it
//!   (`client.cpp`, "Enforce origin ID, except when receiving a snapshot"),
//!   which costs one store and cannot be got around. So do we.
//!
//! - **A frame the server cannot parse does not enter history.** History is
//!   replayed to everyone who joins later, so a message accepted once is
//!   accepted forever. Drawpile treats an unparseable frame as a protocol
//!   violation and drops the connection (`Client::gotBadData`).

use std::fmt;

/// The largest snapshot payload the server will accept.
///
/// A snapshot is a PNG of one layer of one participant's canvas. The largest
/// canvas a session can be created with is 1024×768 (`CANVAS_SIZES`, which the
/// create endpoint now insists on), which is 3 MiB of RGBA before PNG does
/// anything to it; four leaves room for a canvas that compresses badly without
/// leaving the ceiling meaningless.
///
/// This is what bounds a whole checkpoint, and through it the history ceiling:
/// two snapshots per participant, at most `MAX_PARTICIPANTS_CHOICES`'s largest
/// participants, so a checkpoint cannot exceed 64 MiB however pathological.
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;

/// The largest payload any other variable-length message may declare. Text,
/// chat, a fill's coverage bitmap: all far below this, and none of them has a
/// reason to approach it.
const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Why a frame was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// A type this server has no frame layout for. Not fatal: a newer client
    /// reconnecting to an older colour mid-deploy is the ordinary way this
    /// happens, and killing its session over a message it can simply not be
    /// given is worse than ignoring the message.
    UnknownType(u8),
    /// A type we do know, at a length it cannot have. Nothing that speaks this
    /// protocol produces one, so the connection is not speaking it.
    WrongLength {
        msg_type: u8,
        expected: usize,
        actual: usize,
    },
    /// A declared payload past what any real drawing needs.
    TooLarge {
        msg_type: u8,
        declared: usize,
        limit: usize,
    },
}

impl Rejected {
    /// Whether this justifies closing the socket. Only for frames that no
    /// version of the client could have produced.
    pub fn is_fatal(self) -> bool {
        !matches!(self, Rejected::UnknownType(_))
    }
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejected::UnknownType(msg_type) => {
                write!(f, "unknown message type 0x{msg_type:02x}")
            }
            Rejected::WrongLength {
                msg_type,
                expected,
                actual,
            } => write!(
                f,
                "message 0x{msg_type:02x} is {actual} bytes, expected {expected}"
            ),
            Rejected::TooLarge {
                msg_type,
                declared,
                limit,
            } => write!(
                f,
                "message 0x{msg_type:02x} declares {declared} bytes of payload, limit is {limit}"
            ),
        }
    }
}

fn u16_at(data: &[u8], offset: usize) -> Option<usize> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
}

fn u32_at(data: &[u8], offset: usize) -> Option<usize> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
}

/// How long a frame of this type has to be, given what its header declares.
///
/// The layouts are `frontend/collaborate/binaryProtocol.ts`; the encoders there
/// are the definition and these numbers follow them. A variable-length message
/// states its own payload length in the header, so the answer is exact for
/// every type rather than a lower bound — a trailing byte means the sender and
/// this table disagree about the layout, which is worth hearing about.
fn expected_len(data: &[u8]) -> Result<usize, Rejected> {
    let msg_type = data[0];
    // A header too short to read the length field out of is already wrong, and
    // reported against the smallest frame the type can have.
    let variable = |fixed: usize, declared: Option<usize>, limit: usize| match declared {
        Some(declared) if declared > limit => Err(Rejected::TooLarge {
            msg_type,
            declared,
            limit,
        }),
        Some(declared) => Ok(fixed + declared),
        None => Err(Rejected::WrongLength {
            msg_type,
            expected: fixed,
            actual: data.len(),
        }),
    };

    match msg_type {
        // Presence and lifecycle. These carry a 16-byte account UUID, which
        // their handlers check against the authenticated user.
        0x01 => Ok(25),                                            // JOIN
        0x03 => variable(27, u16_at(data, 25), MAX_PAYLOAD_BYTES), // CHAT
        0x04 => Ok(1),                                             // RESET_OFFER
        0x07 => variable(19, u16_at(data, 17), MAX_PAYLOAD_BYTES), // END_SESSION
        0x0c => Ok(11),                                            // RESET_BEGIN
        // A layer of somebody's canvas, as a PNG.
        0x02 => variable(8, u32_at(data, 4), MAX_SNAPSHOT_BYTES), // SNAPSHOT
        // Canvas operations.
        0x12 => Ok(16), // FILL
        0x13 => Ok(2),  // POINTER_UP
        0x14 => Ok(2),  // UNDO_POINT
        0x15 => Ok(3),  // UNDO
        0x16 => match u16_at(data, 10) {
            // 12 bytes of header, four per point, four of trailing mask.
            Some(points) => Ok(16 + points * 4),
            None => Err(Rejected::WrongLength {
                msg_type,
                expected: 16,
                actual: data.len(),
            }),
        }, // STROKE
        0x17 => Ok(22), // REGION
        0x18 => Ok(22), // LINE
        0x19 => Ok(30), // BEZIER
        0x1a => Ok(4),  // ERASE_ALL
        0x1b => variable(19, u16_at(data, 13), MAX_PAYLOAD_BYTES), // TEXT
        0x1c => Ok(10), // MOVE_POINTER
        0x1d => variable(20, u32_at(data, 16), MAX_PAYLOAD_BYTES), // PUT_IMAGE
        _ => Err(Rejected::UnknownType(msg_type)),
    }
}

/// Accepts a frame this server knows how to lay out, at exactly its length.
pub fn validate(data: &[u8]) -> Result<(), Rejected> {
    if data.is_empty() {
        return Err(Rejected::UnknownType(0));
    }
    let expected = expected_len(data)?;
    if data.len() == expected {
        Ok(())
    } else {
        Err(Rejected::WrongLength {
            msg_type: data[0],
            expected,
            actual: data.len(),
        })
    }
}

/// True for the messages whose second byte names their author.
///
/// A snapshot is one of them. It also carries, in its *third* byte, the
/// participant whose layers it depicts — one client uploads everybody's canvas
/// during a session reset, and that is the field which says whose. Only the
/// author is the server's to decide.
fn carries_author(msg_type: u8) -> bool {
    msg_type == 0x02 || (0x12..=0x1d).contains(&msg_type)
}

/// Overwrites the author of a canvas message with the id this connection was
/// given at WELCOME. Returns the id the client claimed, when it was not its
/// own — that is a client drawing as somebody else, and worth a log line.
pub fn enforce_origin(data: &mut [u8], session_user_id: u8) -> Option<u8> {
    if data.len() < 2 || !carries_author(data[0]) {
        return None;
    }
    let claimed = data[1];
    data[1] = session_user_id;
    (claimed != session_user_id).then_some(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames the client encoders actually produce, at the sizes they
    /// produce them. Kept beside the table so a layout change breaks here
    /// rather than in a session.
    fn stroke(points: usize) -> Vec<u8> {
        let mut frame = vec![0x16; 12];
        frame[10..12].copy_from_slice(&(points as u16).to_le_bytes());
        frame.resize(16 + points * 4, 0);
        frame
    }

    #[test]
    fn accepts_every_frame_the_client_encodes() {
        let mut text = vec![0x1b; 15];
        text[13..15].copy_from_slice(&5u16.to_le_bytes());
        text.resize(19 + 5, 0);

        let mut put_image = vec![0x1d; 20];
        put_image[16..20].copy_from_slice(&40u32.to_le_bytes());
        put_image.resize(20 + 40, 0);

        let mut snapshot = vec![0x02; 8];
        snapshot[4..8].copy_from_slice(&256u32.to_le_bytes());
        snapshot.resize(8 + 256, 0);

        let mut chat = vec![0x03; 27];
        chat[25..27].copy_from_slice(&3u16.to_le_bytes());
        chat.resize(27 + 3, 0);

        for frame in [
            vec![0x01; 25],
            vec![0x0c; 11],
            vec![0x12; 16],
            vec![0x13; 2],
            vec![0x14; 2],
            vec![0x15; 3],
            stroke(0),
            stroke(1),
            stroke(300),
            vec![0x17; 22],
            vec![0x18; 22],
            vec![0x19; 30],
            vec![0x1a; 4],
            vec![0x1c; 10],
            text,
            put_image,
            snapshot,
            chat,
        ] {
            assert_eq!(validate(&frame), Ok(()), "rejected 0x{:02x}", frame[0]);
        }
    }

    #[test]
    fn refuses_a_known_type_at_the_wrong_length() {
        // The three-byte fill an early integration harness used to send: a
        // real client never produces one.
        assert!(matches!(
            validate(&[0x12, 1, 0xaa]),
            Err(Rejected::WrongLength { .. })
        ));
        // A stroke whose point count does not match the points that follow.
        let mut truncated = stroke(300);
        truncated.truncate(100);
        assert!(matches!(
            validate(&truncated),
            Err(Rejected::WrongLength { .. })
        ));
    }

    /// The two payloads a client declares in four bytes rather than two: a
    /// header alone can ask the server to expect gigabytes.
    #[test]
    fn refuses_a_payload_past_the_ceiling() {
        let mut snapshot = vec![0x02; 8];
        snapshot[4..8].copy_from_slice(&(MAX_SNAPSHOT_BYTES as u32 + 1).to_le_bytes());
        assert!(matches!(
            validate(&snapshot),
            Err(Rejected::TooLarge { .. })
        ));

        let mut put_image = vec![0x1d; 20];
        put_image[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            validate(&put_image),
            Err(Rejected::TooLarge { .. })
        ));
    }

    /// A header too short to even read its own length field out of.
    #[test]
    fn refuses_a_truncated_header() {
        for frame in [vec![0x02, 0x01], vec![0x1b, 0x01], vec![0x03], vec![0x16]] {
            assert!(matches!(
                validate(&frame),
                Err(Rejected::WrongLength { .. })
            ));
        }
    }

    #[test]
    fn ignores_an_unknown_type_without_closing_the_connection() {
        let rejection = validate(&[0xf0, 1, 2, 3]).expect_err("unknown type");
        assert_eq!(rejection, Rejected::UnknownType(0xf0));
        assert!(!rejection.is_fatal());
    }

    #[test]
    fn a_wrong_length_is_worth_closing_over() {
        assert!(validate(&[0x12, 1, 0xaa])
            .expect_err("short fill")
            .is_fatal());
    }

    #[test]
    fn rewrites_a_forged_author_and_says_so() {
        let mut forged = vec![0x16, 9, 9, 0];
        assert_eq!(enforce_origin(&mut forged, 3), Some(9));
        assert_eq!(forged[1], 3);
        // The third byte is the participant being drawn *on*, which a client
        // is allowed to choose: drawing on somebody else's layers is a feature.
        assert_eq!(forged[2], 9);
    }

    #[test]
    fn leaves_an_honest_author_alone() {
        let mut honest = vec![0x16, 3, 9, 0];
        assert_eq!(enforce_origin(&mut honest, 3), None);
        assert_eq!(honest[1], 3);
    }

    /// A reset upload is one client sending everybody's canvas. The author is
    /// still the uploader, so enforcing it is right; the owner byte beside it
    /// is what carries whose layer each snapshot is, and must survive.
    #[test]
    fn enforces_the_author_of_a_snapshot_but_not_its_subject() {
        let mut snapshot = vec![0x02, 7, 4, 1, 0, 0, 0, 0];
        assert_eq!(enforce_origin(&mut snapshot, 3), Some(7));
        assert_eq!(snapshot[1], 3);
        assert_eq!(snapshot[2], 4);
    }

    #[test]
    fn leaves_messages_that_identify_their_sender_by_uuid_alone() {
        // Chat, join and end-session carry an account UUID that their handlers
        // check against the authenticated user; byte 1 is part of that UUID.
        for msg_type in [0x01, 0x03, 0x07, 0x0c] {
            let mut frame = vec![msg_type, 42, 0, 0];
            assert_eq!(enforce_origin(&mut frame, 3), None);
            assert_eq!(frame[1], 42);
        }
    }
}
