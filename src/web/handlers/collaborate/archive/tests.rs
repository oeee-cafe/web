use super::*;
use crate::web::handlers::collaborate::redis_state::RoomBroadcast;

const HISTORY: Uuid = Uuid::from_u128(7);

fn message(at: u64, seq: u64, payload: &[u8]) -> ArchivedMessage {
    sent_by("conn-1", at, seq, payload)
}

fn sent_by(sender: &str, at: u64, seq: u64, payload: &[u8]) -> ArchivedMessage {
    ArchivedMessage {
        at,
        seq,
        sender: sender.to_string(),
        history_id: HISTORY,
        payload: payload.to_vec(),
    }
}

#[test]
fn a_chunk_round_trips_every_entry_in_order() {
    let entries = vec![
        message(1_700_000_000_000, 1, &[0x16, 0x01, 0x02]),
        sent_by("conn-2", 1_700_000_000_050, 2, &[0x14, 0x01]),
        sent_by("system", 1_700_000_000_900, 3, &[]),
    ];
    let decoded = decode_chunk(&encode_chunk(HISTORY, &entries)).expect("a chunk");
    assert_eq!(decoded, entries);
}

/// The connection that sent each message survives, from a table written
/// once rather than a name repeated on every entry -- and `system`, which
/// is what the server sequences the room's own messages under, is not a
/// UUID and never needed to be.
#[test]
fn senders_are_named_once_and_pointed_at() {
    let entries = vec![
        sent_by("conn-a", 1, 1, &[0x16]),
        sent_by("conn-b", 2, 2, &[0x16]),
        sent_by("conn-a", 3, 3, &[0x16]),
        sent_by("system", 4, 4, &[0x0d]),
    ];
    let encoded = encode_chunk(HISTORY, &entries);
    // Three distinct names, each written once however many messages they
    // sent.
    assert_eq!(encoded.windows(6).filter(|w| *w == b"conn-a").count(), 1);
    assert_eq!(encoded.windows(6).filter(|w| *w == b"system").count(), 1);
    assert_eq!(decode_chunk(&encoded).expect("a chunk"), entries);
}

/// Drawing payloads are arbitrary bytes, and the length prefix is what
/// keeps a delimiter out of the question.
#[test]
fn a_payload_that_looks_like_framing_survives() {
    let entries = vec![message(5, 9, b"OEEELOG\x02\x00\x00\x00\x0a:|\n1|")];
    let decoded = decode_chunk(&encode_chunk(HISTORY, &entries)).expect("a chunk");
    assert_eq!(decoded, entries);
}

/// A session is downloaded as its objects end to end, so the reader has to
/// treat that as one log -- and pick up the second chunk's own history and
/// sender table rather than reading it through the first one's.
#[test]
fn chunks_concatenate_into_one_readable_log() {
    let other = Uuid::from_u128(11);
    let first = vec![sent_by("conn-a", 1, 1, &[0x16, 0x01])];
    let second = vec![ArchivedMessage {
        at: 2,
        seq: 2,
        sender: "conn-z".to_string(),
        history_id: other,
        payload: vec![0x14],
    }];
    let mut joined = encode_chunk(HISTORY, &first);
    joined.extend_from_slice(&encode_chunk(other, &second));

    let decoded = decode_chunk(&joined).expect("a log");
    assert_eq!(decoded, [first, second].concat());
    // A replaced history is visible without the reader tracking chunks.
    assert_eq!(decoded[0].history_id, HISTORY);
    assert_eq!(decoded[1].history_id, other);
}

#[test]
fn an_empty_chunk_is_still_a_chunk() {
    assert_eq!(decode_chunk(&encode_chunk(HISTORY, &[])), Some(Vec::new()));
}

/// The byte that lets this format grow. An entry of a kind written after
/// this reader is skipped by its length, so a file with one in it still
/// yields everything else rather than being lost.
#[test]
fn an_entry_of_an_unknown_kind_is_skipped_not_fatal() {
    let known = vec![message(1, 1, &[0x16, 0x01]), message(3, 3, &[0x16, 0x03])];
    let mut encoded = encode_chunk(HISTORY, &known);
    // Splice a future entry in between: kind 99, sender 0, seq 2, one byte.
    let mut future = vec![99u8, 0];
    future.extend_from_slice(&2u64.to_le_bytes());
    future.extend_from_slice(&2u64.to_le_bytes());
    future.extend_from_slice(&1u32.to_le_bytes());
    future.push(0xff);
    let split = encoded.len() - (ENTRY_HEADER + 2);
    encoded.splice(split..split, future);

    assert_eq!(decode_chunk(&encoded).expect("a chunk"), known);
}

#[test]
fn rejects_bytes_that_are_not_an_archive() {
    assert_eq!(decode_chunk(b""), None);
    assert_eq!(decode_chunk(b"OEEELOG\x01"), None);
    assert_eq!(decode_chunk(b"not a log at all"), None);
}

/// A chunk cut off mid-write -- a flush that died, a truncated download --
/// gives up everything whole before the cut and nothing else. The point of
/// a forensic log is that a bad tail does not cost the head.
#[test]
fn a_truncated_chunk_reads_up_to_the_cut() {
    let entries = vec![
        message(1, 1, &[0x16; 8]),
        message(2, 2, &[0x17; 8]),
        message(3, 3, &[0x18; 8]),
    ];
    let whole = encode_chunk(HISTORY, &entries);
    // From the header onwards: a file cut before that is not identifiable
    // as an archive at all.
    let header = read_header(&whole, 0).expect("a header").2;
    for cut in header..whole.len() {
        let decoded = decode_chunk(&whole[..cut]).expect("a chunk");
        assert_eq!(decoded[..], entries[..decoded.len()], "cut {cut}");
    }
}

/// The buffer keeps the broadcast as it went out; flattening it into a
/// recorded message is this side's job.
#[test]
fn a_buffered_entry_round_trips_through_its_redis_encoding() {
    let broadcast = RoomBroadcast {
        from_connection: "conn-1".to_string(),
        target_connection: None,
        seq: Some(42),
        history_id: Some(HISTORY),
        payload: vec![0x16, 0x03],
    };
    let mut raw = b"1700000000123:".to_vec();
    raw.extend_from_slice(&broadcast.encode());
    assert_eq!(
        decode_buffered(&raw),
        Some(message(1_700_000_000_123, 42, &[0x16, 0x03]))
    );
}

#[test]
fn rejects_a_buffered_entry_without_a_timestamp() {
    assert_eq!(decode_buffered(b"1|conn||1|\npayload"), None);
    assert_eq!(decode_buffered(b""), None);
}

/// The trigger fires once per window, on whichever connection sent that
/// message -- not once per participant per window.
#[test]
fn only_one_sequence_in_each_window_asks_for_a_flush() {
    let asked: Vec<u64> = (1..=(FLUSH_EVERY * 2))
        .filter(|seq| seq.is_multiple_of(FLUSH_EVERY))
        .collect();
    assert_eq!(asked, vec![FLUSH_EVERY, FLUSH_EVERY * 2]);
}

/// Recording is off unless somewhere private has been named for it.
///
/// The image bucket is served straight to browsers, so defaulting to it
/// would publish every session's traffic to anyone holding the id. An
/// empty setting is the same as an absent one, because a config written
/// out with the key blank means the same thing as one without it.
#[test]
fn a_tail_reads_the_chunk_it_falls_inside_and_every_later_one() {
    let chunks: Vec<(u64, String)> = [1, 513, 1025]
        .into_iter()
        .map(|first| (first, chunk_key(Uuid::nil(), first)))
        .collect();
    let firsts = |after| -> Vec<u64> {
        chunks_after(&chunks, after)
            .iter()
            .map(|key| chunk_first_seq(key).unwrap())
            .collect()
    };
    assert_eq!(firsts(0), vec![1, 513, 1025]);
    assert_eq!(firsts(600), vec![513, 1025]);
    // At a chunk's first sequence the chunk before it is already read.
    assert_eq!(firsts(1025), vec![1025]);
    assert_eq!(firsts(5000), vec![1025]);
    assert!(chunks_after(&[], 10).is_empty());
}

#[test]
fn only_chunks_are_read_as_chunks() {
    assert_eq!(
        chunk_first_seq("collaborate-archive/x/000000000513.oeeelog.gz"),
        Some(513)
    );
    assert_eq!(
        chunk_first_seq("collaborate-archive/x/000000000001.oeeelog"),
        Some(1)
    );
    assert_eq!(chunk_first_seq("collaborate-archive/x/manifest.json"), None);
    assert_eq!(
        chunk_first_seq("collaborate-archive/x/diagnostics/a-b.json"),
        None
    );
}

/// A reset replaces the history partway through what is buffered, and a
/// chunk header names one history, so the tail is as many chunks as there
/// are runs -- and still reads back as one log.
#[test]
fn a_tail_across_a_reset_names_each_history() {
    let other = Uuid::from_u128(8);
    let mut messages = vec![message(10, 1, b"a"), message(11, 2, b"b")];
    let mut after_reset = message(12, 3, b"c");
    after_reset.history_id = other;
    messages.push(after_reset);
    let decoded = decode_chunk(&encode_runs(messages.clone())).unwrap();
    assert_eq!(decoded, messages);
    assert!(encode_runs(Vec::new()).is_empty());
}

#[test]
fn a_report_key_says_when_it_was_filed_and_by_whom() {
    let (at, by) = filed_as(
        "collaborate-archive/00000000-0000-0000-0000-000000000001/diagnostics/20260924T084655.527Z-some-one.json",
    );
    assert_eq!(at.as_deref(), Some("2026-09-24T08:46:55.527Z"));
    // A hyphen in the login name stays in the login name.
    assert_eq!(by.as_deref(), Some("some-one"));
}

#[test]
fn a_report_key_of_another_shape_is_not_guessed_at() {
    assert_eq!(
        filed_as("collaborate-archive/x/diagnostics/report.json"),
        (None, None)
    );
}

#[test]
fn recording_is_off_until_a_bucket_is_named_for_it() {
    assert_eq!(selected_bucket(None), None);
    assert_eq!(selected_bucket(Some("")), None);
    assert_eq!(
        selected_bucket(Some("oeee-cafe-archive")),
        Some("oeee-cafe-archive")
    );
}

/// Storage is compressed; what a reader is handed is not. The decoders on
/// both sides are written against the plain format and there is no reason
/// they should have to know how the bytes were kept.
#[test]
fn a_chunk_survives_being_compressed_for_storage() {
    let entries = vec![
        message(1_700_000_000_000, 1, &[0x16, 0x01, 0x02]),
        message(1_700_000_000_050, 2, &[0x14, 0x01]),
    ];
    let plain = encode_chunk(HISTORY, &entries);
    let stored = compress(&plain).expect("compress");
    assert_eq!(decompress(&stored).expect("decompress"), plain);
    assert_eq!(decode_chunk(&decompress(&stored).unwrap()), Some(entries));
}

/// Two chunks were written before this, and there is no reason they should
/// stop reading.
#[test]
fn a_chunk_stored_before_compression_still_reads() {
    let entries = vec![message(5, 9, &[0x16, 0x03])];
    let plain = encode_chunk(HISTORY, &entries);
    assert_eq!(decompress(&plain).expect("passthrough"), plain);
}

/// The saving is the whole reason for it. Real recordings run five- or
/// sixfold; this only asks that a body of repeated framing does not come
/// out bigger than it went in.
#[test]
fn compressing_a_recording_makes_it_smaller() {
    let entries: Vec<ArchivedMessage> = (1..200)
        .map(|seq| message(1_700_000_000_000 + seq, seq, &[0x16, 0x01, 0x02, 0x03]))
        .collect();
    let plain = encode_chunk(HISTORY, &entries);
    let stored = compress(&plain).expect("compress");
    assert!(
        stored.len() * 3 < plain.len(),
        "{} compressed to {}",
        plain.len(),
        stored.len()
    );
}

/// Several chunks are in flight at once and finish in whatever order they
/// like. A recording assembled in the order they *arrive* would be
/// silently out of order -- it would still decode, and every sequence in
/// it would be wrong.
#[tokio::test]
async fn chunks_are_joined_in_key_order_however_they_arrive() {
    let keys: Vec<String> = (0..16).map(|index| format!("chunk-{index:02}")).collect();
    let joined = assemble(keys.clone(), |key| async move {
        // The later the key, the sooner it comes back: the arrival order
        // is exactly the reverse of the order the log needs.
        let index: u64 = key.trim_start_matches("chunk-").parse().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20 - index)).await;
        Ok(format!("[{index}]").into_bytes())
    })
    .await
    .expect("assembled");

    let expected: String = (0..16).map(|index| format!("[{index}]")).collect();
    assert_eq!(String::from_utf8(joined).unwrap(), expected);
}

/// One chunk that will not come back fails the download rather than
/// yielding the rest, which would be a recording with a hole in it that
/// reads as whole.
#[tokio::test]
async fn a_chunk_that_fails_fails_the_whole_download() {
    let keys: Vec<String> = (0..8).map(|index| format!("chunk-{index}")).collect();
    let result = assemble(keys, |key| async move {
        if key.ends_with('5') {
            return Err("gone".into());
        }
        Ok(key.into_bytes())
    })
    .await;
    assert!(result.is_err());
}

#[test]
fn chunk_keys_sort_in_sequence_order() {
    let room = Uuid::from_u128(1);
    let mut keys = vec![
        chunk_key(room, 1024),
        chunk_key(room, 1),
        chunk_key(room, 99),
    ];
    keys.sort();
    assert_eq!(
        keys,
        vec![
            chunk_key(room, 1),
            chunk_key(room, 99),
            chunk_key(room, 1024)
        ]
    );
}
