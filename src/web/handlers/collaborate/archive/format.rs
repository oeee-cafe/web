//! The recording's bytes: a chunk of messages, and how it is compressed.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use uuid::Uuid;

use crate::web::handlers::collaborate::redis_state::RoomBroadcast;

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
pub(super) const ENTRY_HEADER: usize = 1 + 1 + 8 + 8 + 4;

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
pub(super) fn read_header(bytes: &[u8], at: usize) -> Option<(Uuid, Vec<String>, usize)> {
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
pub(super) fn decode_buffered(entry: &[u8]) -> Option<ArchivedMessage> {
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
pub(super) async fn decompress_off_thread(stored: Vec<u8>) -> Result<Vec<u8>, std::io::Error> {
    tokio::task::spawn_blocking(move || decompress(&stored))
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
}

/// Restores one, or passes it through when it was stored before compression.
///
/// Decompressed here rather than handed on compressed: what a reader gets is
/// the format its decoder is written against, in both languages, and the
/// verified reader stays untouched by how the bytes happened to be kept.
pub(super) fn decompress(stored: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    if stored.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        GzDecoder::new(stored).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(stored.to_vec())
    }
}

/// Buffered messages as chunks, one per run of the same history. A reset
/// replaces the history mid-buffer, and a chunk header names one.
pub(super) fn encode_runs(messages: Vec<ArchivedMessage>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut start = 0;
    for end in 1..=messages.len() {
        if end == messages.len() || messages[end].history_id != messages[start].history_id {
            out.extend(encode_chunk(
                messages[start].history_id,
                &messages[start..end],
            ));
            start = end;
        }
    }
    out
}
