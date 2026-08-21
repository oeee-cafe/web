use std::process::{Child, Command, Stdio};
use std::time::Duration;

use axum::extract::ws::Message as AxumMessage;
use bb8_redis::{bb8::Pool, RedisConnectionManager};
use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};
use uuid::Uuid;

use super::archive::ArchiveBuffer;
use super::preview::{ImageKind, PreviewStore};
use super::redis_messages::{self, RedisMessageStore, Sequenced};
use super::redis_state::{RedisStateManager, RoomBroadcast};
use super::room_fanout::RoomFanout;
use super::websocket::{valid_reset_payloads, wrap_sequenced};
use crate::redis::RedisPool;

const SEQUENCED: u8 = 0x0a;
const CAUGHT_UP: u8 = 0x0f;

struct RedisProcess(Option<Child>);

impl Drop for RedisProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Harness {
    _redis: RedisProcess,
    server: JoinHandle<()>,
    url: String,
    pool: RedisPool,
    room: Uuid,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Position {
    history_id: Uuid,
    sequence: u64,
    payload: Vec<u8>,
}

fn unused_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral address")
        .port()
}

async fn start_redis() -> (RedisProcess, String) {
    if let Ok(url) = std::env::var("OEEE_TEST_REDIS_URL") {
        wait_for_redis(&url).await;
        return (RedisProcess(None), url);
    }
    let port = unused_port();
    let child = Command::new("redis-server")
        .args([
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--save",
            "",
            "--appendonly",
            "no",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("redis-server must be installed for collaboration protocol tests");
    let redis = RedisProcess(Some(child));
    let url = format!("redis://127.0.0.1:{port}/");
    wait_for_redis(&url).await;
    (redis, url)
}

async fn wait_for_redis(url: &str) {
    let client = redis::Client::open(url).expect("valid Redis URL");
    for _ in 0..100 {
        if client.get_multiplexed_async_connection().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("collaboration test Redis did not become ready at {url}");
}

async fn redis_pool(url: &str) -> RedisPool {
    let manager = RedisConnectionManager::new(url).expect("Redis manager");
    Pool::builder().build(manager).await.expect("Redis pool")
}

fn room_message(payload: Vec<u8>) -> RoomBroadcast {
    RoomBroadcast {
        from_connection: Uuid::new_v4().to_string(),
        target_connection: None,
        seq: None,
        history_id: None,
        payload,
    }
}

async fn serve_connection(stream: TcpStream, pool: RedisPool, redis_url: String, room: Uuid) {
    let socket = accept_async(stream).await.expect("WebSocket handshake");
    let (mut sink, mut source) = socket.split();
    let channel = format!("oeee:pubsub:{room}");
    let client = redis::Client::open(redis_url).expect("Redis client");
    let mut subscriber = client.get_async_pubsub().await.expect("Redis PubSub");
    subscriber
        .subscribe(&channel)
        .await
        .expect("subscribe room");

    // Subscribe before taking the snapshot. The live feed may overlap replay,
    // and the client can deduplicate that overlap by canonical position.
    let store = RedisMessageStore::new(pool.clone());
    let (replay_history_id, history) = store
        .get_history_snapshot(room)
        .await
        .expect("history snapshot");
    let mut last_sequence = 0;
    for (sequence, message) in history {
        if let AxumMessage::Binary(payload) = message {
            sink.send(Message::Binary(
                wrap_sequenced(replay_history_id, sequence, &payload).into(),
            ))
            .await
            .expect("send replay");
            last_sequence = sequence;
        }
    }
    let mut caught_up = vec![CAUGHT_UP];
    caught_up.extend_from_slice(replay_history_id.as_bytes());
    caught_up.extend_from_slice(&last_sequence.to_le_bytes());
    sink.send(Message::Binary(caught_up.into()))
        .await
        .expect("send caught-up marker");

    let outgoing = tokio::spawn(async move {
        let mut messages = subscriber.on_message();
        while let Some(message) = messages.next().await {
            let framed: Vec<u8> = message.get_payload().expect("PubSub frame");
            let envelope = RoomBroadcast::decode(&framed).expect("room envelope");
            if let (Some(history_id), Some(sequence)) = (envelope.history_id, envelope.seq) {
                if history_id == replay_history_id && sequence <= last_sequence {
                    continue;
                }
                if sink
                    .send(Message::Binary(
                        wrap_sequenced(history_id, sequence, &envelope.payload).into(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });

    while let Some(Ok(Message::Binary(payload))) = source.next().await {
        let envelope = room_message(payload.to_vec());
        store
            .sequence_and_publish(room, &payload, &envelope.from_connection, &channel)
            .await
            .expect("sequence client operation");
    }
    outgoing.abort();
}

async fn start_harness() -> Harness {
    let (redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;
    let room = Uuid::new_v4();
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("listener address");
    let server_pool = pool.clone();
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_connection(
                stream,
                server_pool.clone(),
                redis_url.clone(),
                room,
            ));
        }
    });
    Harness {
        _redis: redis,
        server,
        url: format!("ws://{address}"),
        pool,
        room,
    }
}

fn decode_position(message: Message) -> Position {
    let Message::Binary(bytes) = message else {
        panic!("expected binary WebSocket message")
    };
    assert_eq!(bytes[0], SEQUENCED);
    Position {
        history_id: Uuid::from_slice(&bytes[1..17]).expect("history UUID"),
        sequence: u64::from_le_bytes(bytes[17..25].try_into().expect("sequence")),
        payload: bytes[25..].to_vec(),
    }
}

async fn caught_up<S>(socket: &mut S) -> (Uuid, u64)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .expect("socket remains open")
            .expect("WebSocket message");
        let Message::Binary(bytes) = message else {
            continue;
        };
        if bytes.first() == Some(&CAUGHT_UP) {
            return (
                Uuid::from_slice(&bytes[1..17]).expect("history UUID"),
                u64::from_le_bytes(bytes[17..25].try_into().expect("last sequence")),
            );
        }
    }
}

async fn replay_through_caught_up<S>(socket: &mut S) -> (Vec<Position>, Uuid, u64)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut replay = Vec::new();
    loop {
        let message = socket
            .next()
            .await
            .expect("socket remains open")
            .expect("WebSocket message");
        let Message::Binary(bytes) = &message else {
            continue;
        };
        if bytes.first() == Some(&CAUGHT_UP) {
            return (
                replay,
                Uuid::from_slice(&bytes[1..17]).expect("history UUID"),
                u64::from_le_bytes(bytes[17..25].try_into().expect("last sequence")),
            );
        }
        replay.push(decode_position(message));
    }
}

#[tokio::test]
async fn multiple_clients_share_one_canonical_order_and_late_join_replay() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let (mut alice, _) = connect_async(&harness.url).await.expect("Alice connects");
        let (mut bob, _) = connect_async(&harness.url).await.expect("Bob connects");
        let alice_start = caught_up(&mut alice).await;
        let bob_start = caught_up(&mut bob).await;
        assert_eq!(alice_start, bob_start);
        assert_eq!(alice_start.1, 0);

        alice
            .send(Message::Binary(vec![0x12, 1, 0xaa].into()))
            .await
            .expect("Alice draws");
        bob.send(Message::Binary(vec![0x12, 2, 0xbb].into()))
            .await
            .expect("Bob draws");

        let mut alice_history = Vec::new();
        let mut bob_history = Vec::new();
        for _ in 0..2 {
            alice_history.push(decode_position(
                alice
                    .next()
                    .await
                    .unwrap()
                    .expect("Alice receives operation"),
            ));
            bob_history.push(decode_position(
                bob.next().await.unwrap().expect("Bob receives operation"),
            ));
        }
        assert_eq!(alice_history, bob_history);
        assert_eq!(
            alice_history
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(alice_history
            .iter()
            .all(|entry| entry.history_id == alice_start.0));

        let (mut late_joiner, _) = connect_async(&harness.url)
            .await
            .expect("late joiner connects");
        let replay = vec![
            decode_position(late_joiner.next().await.unwrap().unwrap()),
            decode_position(late_joiner.next().await.unwrap().unwrap()),
        ];
        assert_eq!(replay, alice_history);
        assert_eq!(caught_up(&mut late_joiner).await, (alice_start.0, 2));
    })
    .await
    .expect("collaboration protocol scenario timed out");
}

/// A flood fill, which travels as a rectangle of compressed coverage rather
/// than as a seed point, and is by far the largest thing we put on the wire.
///
/// What this reaches is the store: real sequencing, real history, real
/// late-join replay. It does not reach the connection loop -- `serve_connection`
/// above is the harness's own, so who gets an echo is settled by the unit tests
/// in `websocket.rs` and not here.
///
/// So the claim is a narrow one, and it is about size. Every other test in this
/// file sequences three bytes. A fill is kilobytes, and nothing until now has
/// pushed one through Redis storage and got it back byte for byte, in order,
/// with a mark on each side of it.
#[tokio::test]
async fn a_fill_sized_payload_survives_sequencing_and_replay_intact() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let (mut alice, _) = connect_async(&harness.url).await.expect("Alice connects");
        let start = caught_up(&mut alice).await;

        // [0x1d][author][target][layer][x:2][y:2][w:2][h:2][rgba:4][len:4][coverage]
        let mut fill = vec![0x1d, 1, 7, 0];
        fill.extend_from_slice(&5u16.to_le_bytes());
        fill.extend_from_slice(&6u16.to_le_bytes());
        fill.extend_from_slice(&600u16.to_le_bytes());
        fill.extend_from_slice(&800u16.to_le_bytes());
        fill.extend_from_slice(&[200, 100, 50, 255]);
        // A coverage mask the size a full-canvas fill deflates to, and not a
        // run of one byte: Redis is told this is binary, and a payload that
        // happens to be uniform would not notice if it were not.
        let coverage: Vec<u8> = (0..16_384u32)
            .map(|index| (index.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        fill.extend_from_slice(&(coverage.len() as u32).to_le_bytes());
        fill.extend_from_slice(&coverage);

        let before = vec![0x16, 1, 0xaa];
        let after = vec![0x16, 1, 0xbb];
        for payload in [&before, &fill, &after] {
            alice
                .send(Message::Binary(payload.clone().into()))
                .await
                .expect("Alice draws");
        }

        let live: Vec<Position> = {
            let mut seen = Vec::new();
            for _ in 0..3 {
                seen.push(decode_position(alice.next().await.unwrap().expect("mark")));
            }
            seen
        };
        assert_eq!(
            live.iter().map(|entry| entry.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the fill did not take an ordinary place in the order"
        );
        // Compared by hand rather than with `assert_eq!`: these are kilobytes,
        // and a failure that prints both of them in full is a failure nobody
        // reads. Length first, because truncation is the likely way to fail.
        assert_eq!(
            live[1].payload.len(),
            fill.len(),
            "the fill came back a different size"
        );
        let differs = live[1]
            .payload
            .iter()
            .zip(&fill)
            .position(|(got, want)| got != want);
        assert!(
            differs.is_none(),
            "the fill was corrupted in storage, first at byte {:?}",
            differs
        );

        // And the same again for someone arriving afterwards, who gets it out
        // of history rather than off the wire.
        let (mut late, _) = connect_async(&harness.url)
            .await
            .expect("late joiner connects");
        let replayed: Vec<Position> = {
            let mut seen = Vec::new();
            for _ in 0..3 {
                seen.push(decode_position(late.next().await.unwrap().expect("replay")));
            }
            seen
        };
        assert!(replayed == live, "replay did not match what was broadcast");
        assert_eq!(caught_up(&mut late).await, (start.0, 3));
    })
    .await
    .expect("collaboration protocol scenario timed out");
}

#[tokio::test]
async fn reconnect_replays_missed_operations_once_and_reports_exact_position() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let (mut client, _) = connect_async(&harness.url).await.expect("client connects");
        let initial = caught_up(&mut client).await;

        client
            .send(Message::Binary(vec![0x12, 1, 0x11].into()))
            .await
            .expect("first operation");
        assert_eq!(
            decode_position(client.next().await.unwrap().unwrap()).sequence,
            1
        );
        client.close(None).await.expect("disconnect client");

        let store = RedisMessageStore::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);
        let payload = vec![0x12, 2, 0x22];
        let envelope = room_message(payload.clone());
        store
            .sequence_and_publish(harness.room, &payload, &envelope.from_connection, &channel)
            .await
            .expect("operation while disconnected");

        let (mut reconnected, _) = connect_async(&harness.url)
            .await
            .expect("client reconnects");
        let (replay, history_id, last_sequence) = replay_through_caught_up(&mut reconnected).await;
        assert_eq!(history_id, initial.0);
        assert_eq!(last_sequence, 2);
        assert_eq!(
            replay
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(replay[1].payload, payload);

        // No replay entry is delivered again through the overlapping PubSub
        // subscription after CAUGHT_UP.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), reconnected.next())
                .await
                .is_err()
        );
    })
    .await
    .expect("reconnect scenario timed out");
}

/// The window is what paces previews, and it opens for exactly one client.
///
/// Without this every participant would render and upload the same canvas on
/// the same timer -- the expensive half of the work, done N times for one
/// picture.
#[tokio::test]
async fn only_one_client_holds_a_preview_window_at_a_time() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = PreviewStore::new(harness.pool.clone());

        let first = store.claim(harness.room).await.expect("claim");
        let second = store.claim(harness.room).await.expect("claim again");
        assert!(first.is_some());
        assert_eq!(second, None);

        // A different room is a different window; one busy room must not stop
        // the rest of the lobby refreshing.
        assert!(store.claim(Uuid::new_v4()).await.expect("claim").is_some());
    })
    .await
    .expect("preview claim scenario timed out");
}

/// The token is what makes an upload attributable to a window. A client that
/// claimed, took too long, and came back with a canvas from a minute ago must
/// not overwrite whatever the room agreed on since.
#[tokio::test]
async fn a_preview_is_stored_only_against_the_token_that_claimed_it() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = PreviewStore::new(harness.pool.clone());
        let token = store.claim(harness.room).await.expect("claim").expect("token");

        assert_eq!(
            store
                .store(harness.room, "not-the-token", ImageKind::Webp, b"nope")
                .await
                .expect("store with a wrong token"),
            None
        );
        assert!(store.load(harness.room).await.expect("load").is_none());

        let version = store
            .store(harness.room, &token, ImageKind::Webp, b"first")
            .await
            .expect("store")
            .expect("stored");

        // Spent, not merely used: the same token cannot deliver twice, so a
        // retry after a response that got lost cannot land on top of a newer
        // preview.
        assert_eq!(
            store
                .store(harness.room, &token, ImageKind::Webp, b"second")
                .await
                .expect("store again"),
            None
        );

        let stored = store.load(harness.room).await.expect("load").expect("preview");
        assert_eq!(stored.bytes, b"first");
        assert_eq!(stored.kind, ImageKind::Webp);
        assert_eq!(stored.version, version);
        assert_eq!(
            store.version(harness.room).await.expect("version"),
            Some(version)
        );
    })
    .await
    .expect("preview store scenario timed out");
}

/// The lobby asks about every card it is rendering at once, and has to be able
/// to tell "no preview" from "preview" per room -- it decides whether to emit
/// an `<img>` at all from this.
#[tokio::test]
async fn preview_versions_come_back_per_room_in_the_order_asked() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = PreviewStore::new(harness.pool.clone());
        let empty = Uuid::new_v4();
        let other = Uuid::new_v4();

        let token = store.claim(harness.room).await.expect("claim").expect("token");
        let version = store
            .store(harness.room, &token, ImageKind::Png, b"drawing")
            .await
            .expect("store")
            .expect("stored");
        let other_token = store.claim(other).await.expect("claim").expect("token");
        let other_version = store
            .store(other, &other_token, ImageKind::Png, b"drawing")
            .await
            .expect("store")
            .expect("stored");

        assert_eq!(
            store
                .versions(&[empty, harness.room, other])
                .await
                .expect("versions"),
            vec![None, Some(version), Some(other_version)]
        );
        assert_eq!(store.versions(&[]).await.expect("versions"), Vec::new());

        store.cleanup(harness.room).await.expect("cleanup");
        assert_eq!(
            store.versions(&[harness.room]).await.expect("versions"),
            vec![None]
        );
        assert!(store.load(harness.room).await.expect("load").is_none());
        // Cleanup releases the window too, so a session id that somehow comes
        // back is not locked out by a claim nobody can spend.
        assert!(store.claim(harness.room).await.expect("claim").is_some());
    })
    .await
    .expect("preview versions scenario timed out");
}

/// The recording is written by the sequencer, in the step that assigns the
/// position -- so a message that is canonical is recorded, once, in that
/// order. Anything less would leave the one message a post-mortem wants
/// missing: the one that was accepted just before things went wrong.
#[tokio::test]
async fn every_sequenced_message_lands_in_the_archive_in_order() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        for index in 0..8u8 {
            store
                .sequence_and_publish(harness.room, &[0x16, index], "conn-a", &channel)
                .await
                .expect("sequence");
        }

        assert_eq!(buffer.pending(harness.room).await.expect("pending"), 8);
        let entries = buffer.peek(harness.room, 100).await.expect("peek");
        assert_eq!(entries.len(), 8);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(Some(entry.seq), Some(index as u64 + 1));
            assert_eq!(entry.payload, vec![0x16, index as u8]);
            // Which connection sent it, which is the attribution the live
            // history does not keep.
            assert_eq!(entry.sender, "conn-a");
            assert!(entry.at > 0, "a recorded message is stamped");
        }
        // Non-decreasing, so a replay can pace itself by them.
        for pair in entries.windows(2) {
            assert!(pair[1].at >= pair[0].at);
        }
    })
    .await
    .expect("archive ordering scenario timed out");
}

/// A refused message never became canonical, so it must not appear to have
/// been. The archive is evidence, and evidence of a message the room never
/// saw is worse than no evidence.
#[tokio::test]
async fn a_message_the_history_refused_is_not_recorded() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        let payload = vec![0x16; 1024 * 1024];
        let mut refused = false;
        for _ in 0..(redis_messages::MAX_HISTORY_BYTES / payload.len() as u64) + 2 {
            match store
                .sequence_and_publish(harness.room, &payload, "conn-a", &channel)
                .await
                .expect("sequence")
            {
                Sequenced::HistoryFull { .. } => {
                    refused = true;
                    break;
                }
                Sequenced::Stored { .. } => continue,
            }
        }
        assert!(refused, "the history reached its ceiling");

        // One entry per accepted message and none for the refusal.
        let recorded = buffer.pending(harness.room).await.expect("pending");
        let sequence = store
            .current_sequence(harness.room)
            .await
            .expect("sequence")
            .expect("a position");
        assert_eq!(recorded as u64, sequence);
    })
    .await
    .expect("archive refusal scenario timed out");
}

/// Entries are read before they are written out and dropped only once they
/// are stored, so a flush that dies in between repeats a chunk rather than
/// losing one -- and only one flusher gets to do it at a time.
#[tokio::test]
async fn the_archive_buffer_is_drained_only_by_the_flusher_that_claimed_it() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        for index in 0..5u8 {
            store
                .sequence_and_publish(harness.room, &[0x16, index], "conn-a", &channel)
                .await
                .expect("sequence");
        }

        assert!(buffer.claim(harness.room).await.expect("claim"));
        assert!(!buffer.claim(harness.room).await.expect("claim again"));

        // Peeking leaves everything where it is: nothing is lost if the write
        // that follows never happens.
        let entries = buffer.peek(harness.room, 3).await.expect("peek");
        assert_eq!(entries.len(), 3);
        assert_eq!(buffer.pending(harness.room).await.expect("pending"), 5);

        buffer.drop_front(harness.room, 3).await.expect("drop");
        let rest = buffer.peek(harness.room, 100).await.expect("peek");
        assert_eq!(rest.len(), 2);
        assert_eq!(Some(rest[0].seq), Some(4));

        buffer.release(harness.room).await.expect("release");
        assert!(buffer.claim(harness.room).await.expect("claim after release"));
    })
    .await
    .expect("archive flush claim scenario timed out");
}

/// The sweeper revisits every ended session on every pass, so anything it
/// does per session it does forever. Sealing a room with nothing buffered
/// wrote a manifest each time -- thousands of objects an hour describing
/// sessions that had no recording at all.
#[tokio::test]
async fn a_room_with_nothing_buffered_has_nothing_left_to_seal() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        // A room nobody ever drew in.
        assert_eq!(buffer.pending(harness.room).await.expect("pending"), 0);

        // One that did, and has not been flushed.
        store
            .sequence_and_publish(harness.room, &[0x16, 0x01], "conn-a", &channel)
            .await
            .expect("sequence");
        assert_eq!(buffer.pending(harness.room).await.expect("pending"), 1);

        // And once its entries are gone, it is finished with: an emptied
        // buffer is what says the recording has already been written out.
        buffer.drop_front(harness.room, 1).await.expect("drop");
        assert_eq!(buffer.pending(harness.room).await.expect("pending"), 0);
    })
    .await
    .expect("seal skipping scenario timed out");
}

/// Chat never reaches the sequencer, so it is not in the log and has to be
/// kept beside it -- and a room that talked and never drew is still a room
/// with something worth keeping.
#[tokio::test]
async fn what_was_said_is_kept_apart_from_what_was_drawn() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        store
            .sequence_and_publish(harness.room, &[0x16, 0x01], "conn-a", &channel)
            .await
            .expect("sequence");
        store
            .append_chat_message(harness.room, &[0x03, 0x01])
            .await
            .expect("chat");

        // The drawing buffer holds the stroke and nothing else: a chat line in
        // there would be a message that is not a mark in the stream a canvas
        // is rebuilt from.
        let entries = buffer.peek(harness.room, 100).await.expect("peek");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].payload, vec![0x16, 0x01]);
    })
    .await
    .expect("chat separation scenario timed out");
}

/// The recording outlives the working set. A checkpoint squashes history at
/// its base and the room keeps drawing; the archive still holds every message
/// from before it, which is the whole reason it exists.
#[tokio::test]
async fn a_checkpoint_compacts_history_and_leaves_the_archive_whole() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let buffer = ArchiveBuffer::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        for index in 0..6u8 {
            store
                .sequence_and_publish(harness.room, &[0x16, index], "conn-a", &channel)
                .await
                .expect("sequence");
        }
        store
            .apply_reset(harness.room, 4, &[vec![0x02, 1, 1, 0]])
            .await
            .expect("apply reset");

        // History has given up everything at or below the base.
        let history = store.get_history(harness.room).await.expect("history");
        assert!(history.len() < 6, "the checkpoint compacted the history");

        // The recording has not.
        let entries = buffer.peek(harness.room, 100).await.expect("peek");
        let recorded: Vec<u64> = entries.iter().map(|entry| entry.seq).collect();
        assert_eq!(recorded, vec![1, 2, 3, 4, 5, 6]);
    })
    .await
    .expect("archive survives compaction scenario timed out");
}

#[tokio::test]
async fn recent_chat_is_bounded_and_removed_with_the_room() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        for index in 0..105u8 {
            store
                .append_chat_message(harness.room, &[0x03, index])
                .await
                .expect("append recent chat");
        }

        let chat = store.get_recent_chat(harness.room).await.expect("recent chat");
        assert_eq!(chat.len(), 100);
        assert_eq!(chat.first(), Some(&vec![0x03, 5]));
        assert_eq!(chat.last(), Some(&vec![0x03, 104]));

        store.cleanup_room(harness.room).await.expect("cleanup room");
        assert!(store.get_recent_chat(harness.room).await.unwrap().is_empty());
    })
    .await
    .expect("recent chat scenario timed out");
}

#[tokio::test]
async fn checkpoint_compaction_keeps_operations_that_race_after_its_base() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let store = RedisMessageStore::new(harness.pool.clone());
        let channel = format!("oeee:pubsub:{}", harness.room);

        for marker in [0x11, 0x22] {
            let payload = vec![0x12, 1, marker];
            let envelope = room_message(payload.clone());
            store
                .sequence_and_publish(harness.room, &payload, &envelope.from_connection, &channel)
                .await
                .expect("seed canonical operation");
        }

        let snapshots = [vec![0x02, 1, 1, 0xaa], vec![0x02, 1, 0, 0xbb]];
        let newer_payload = vec![0x12, 2, 0x33];
        let newer_envelope = room_message(newer_payload.clone());
        let publish_newer = store.sequence_and_publish(
            harness.room,
            &newer_payload,
            &newer_envelope.from_connection,
            &channel,
        );
        let apply_checkpoint = store.apply_reset(harness.room, 2, &snapshots);
        let (published, compacted) = tokio::join!(publish_newer, apply_checkpoint);
        assert!(matches!(
            published.expect("newer operation sequenced"),
            Sequenced::Stored { seq: 3, .. }
        ));
        compacted.expect("checkpoint applied");

        let (mut late_joiner, _) = connect_async(&harness.url)
            .await
            .expect("late joiner connects");
        let (replay, _, last_sequence) = replay_through_caught_up(&mut late_joiner).await;
        assert_eq!(last_sequence, 3);
        assert_eq!(
            replay
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![2, 2, 3]
        );
        assert_eq!(replay[0].payload, snapshots[0]);
        assert_eq!(replay[1].payload, snapshots[1]);
        assert_eq!(replay[2].payload, newer_payload);
    })
    .await
    .expect("checkpoint race scenario timed out");
}

#[tokio::test]
async fn future_checkpoint_is_rejected_without_mutating_history() {
    let harness = start_harness().await;
    let store = RedisMessageStore::new(harness.pool.clone());
    let channel = format!("oeee:pubsub:{}", harness.room);
    let payload = vec![0x12, 1, 0x11];
    let envelope = room_message(payload.clone());
    store
        .sequence_and_publish(harness.room, &payload, &envelope.from_connection, &channel)
        .await
        .expect("seed operation");

    let snapshots = [vec![0x02, 1, 1], vec![0x02, 1, 0]];
    assert!(store
        .apply_reset(harness.room, 99, &snapshots)
        .await
        .is_err());
    let (_, history) = store
        .get_history_snapshot(harness.room)
        .await
        .expect("history remains readable");
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0], (1, AxumMessage::Binary(bytes)) if bytes == &payload));
}

/// A checkpoint is most of what a busy room's history weighs, so what the
/// server measures for auto-reset is growth on top of it. Get this wrong and a
/// room either asks for a checkpoint again the instant one lands, or -- with
/// the base never moving -- stops asking altogether.
#[tokio::test]
async fn a_checkpoint_becomes_the_weight_that_later_growth_is_measured_from() {
    let harness = start_harness().await;
    let store = RedisMessageStore::new(harness.pool.clone());
    let channel = format!("oeee:pubsub:{}", harness.room);

    for marker in [0x11, 0x22, 0x33] {
        let payload = vec![0x12, 1, marker];
        store
            .sequence_and_publish(harness.room, &payload, "alice", &channel)
            .await
            .expect("seed operation");
    }
    let before = store.history_size(harness.room).await.expect("size");
    assert_eq!(before.messages, 3);
    assert_eq!(before.bytes, 9);
    // Nothing has been checkpointed, so all of it is growth.
    assert_eq!(before.messages_since_reset(), 3);
    assert_eq!(before.bytes_since_reset(), 9);

    // A checkpoint heavier than the history it replaces, which is the normal
    // case: ours are PNGs of every layer of every participant.
    let snapshots = [vec![0x02; 40], vec![0x02; 60]];
    store
        .apply_reset(harness.room, 3, &snapshots)
        .await
        .expect("checkpoint applied");

    let after = store.history_size(harness.room).await.expect("size");
    assert_eq!(after.messages, 2);
    assert_eq!(after.bytes, 100);
    // The checkpoint is the new floor, not new growth. A room that has just
    // been checkpointed has grown by nothing.
    assert_eq!(after.messages_since_reset(), 0);
    assert_eq!(after.bytes_since_reset(), 0);
    assert!(after.bytes <= redis_messages::effective_auto_reset_bytes(after.base_bytes));

    // And what is drawn after it counts again, from there.
    store
        .sequence_and_publish(harness.room, &[0x12, 1, 0x44], "alice", &channel)
        .await
        .expect("operation after the checkpoint");
    let grown = store.history_size(harness.room).await.expect("size");
    assert_eq!(grown.messages_since_reset(), 1);
    assert_eq!(grown.bytes_since_reset(), 3);
}

/// The wall refuses the newest message. It does not make room by dropping the
/// oldest, because the oldest is the checkpoint every later message is drawn on
/// top of -- a history trimmed from the front replays into a canvas that never
/// existed, and does it silently.
#[tokio::test]
async fn a_full_history_refuses_new_operations_instead_of_dropping_old_ones() {
    let harness = start_harness().await;
    let store = RedisMessageStore::new(harness.pool.clone());
    let channel = format!("oeee:pubsub:{}", harness.room);

    let checkpoint = vec![0x02, 1, 0, 0];
    store
        .sequence_and_publish(harness.room, &checkpoint, "alice", &channel)
        .await
        .expect("seed the checkpoint");

    // Stand the room one byte short of its ceiling rather than actually
    // writing two hundred megabytes through Redis to get there. The counter is
    // what the sequencer reads, so this is the same state, arrived at cheaply.
    let mut conn = harness.pool.get().await.expect("Redis connection");
    let _: () = conn
        .set(
            format!("oeee:msg_bytes:v4:{}", harness.room),
            redis_messages::MAX_HISTORY_BYTES - 1,
        )
        .await
        .expect("stand the room at its ceiling");

    let refused = store
        .sequence_and_publish(harness.room, &[0x12, 1, 0x55], "alice", &channel)
        .await
        .expect("the ceiling is not an error");
    assert!(matches!(refused, Sequenced::HistoryFull { .. }));

    // The refused message was not stored, not sequenced, and -- the point of
    // all this -- took nothing with it.
    let (_, history) = store
        .get_history_snapshot(harness.room)
        .await
        .expect("history remains readable");
    assert_eq!(history.len(), 1);
    assert!(matches!(&history[0], (1, AxumMessage::Binary(bytes)) if bytes == &checkpoint));

    // A checkpoint is what gets a room out of this, and it puts the counter
    // back to what the checkpoint itself weighs.
    store
        .apply_reset(harness.room, 1, &[vec![0x02; 10], vec![0x02; 10]])
        .await
        .expect("checkpoint applied");
    assert!(matches!(
        store
            .sequence_and_publish(harness.room, &[0x12, 1, 0x66], "alice", &channel)
            .await
            .expect("drawing resumes"),
        Sequenced::Stored { .. }
    ));
}

#[tokio::test]
async fn replacing_history_changes_identity_instead_of_reusing_sequence_space() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let harness = start_harness().await;
        let (mut observer, _) = connect_async(&harness.url)
            .await
            .expect("observer connects");
        let old_position = caught_up(&mut observer).await;

        let store = RedisMessageStore::new(harness.pool.clone());
        store
            .cleanup_room(harness.room)
            .await
            .expect("replace history");
        let channel = format!("oeee:pubsub:{}", harness.room);
        let payload = vec![0x12, 1, 0x44];
        let envelope = room_message(payload.clone());
        store
            .sequence_and_publish(harness.room, &payload, &envelope.from_connection, &channel)
            .await
            .expect("first operation in replacement history");

        let replacement = decode_position(observer.next().await.unwrap().unwrap());
        assert_eq!(replacement.sequence, 1);
        assert_ne!(replacement.history_id, old_position.0);

        let (mut late_joiner, _) = connect_async(&harness.url)
            .await
            .expect("late joiner connects to replacement history");
        let (replay, history_id, last_sequence) = replay_through_caught_up(&mut late_joiner).await;
        assert_eq!(history_id, replacement.history_id);
        assert_eq!(last_sequence, 1);
        assert_eq!(replay, vec![replacement]);
    })
    .await
    .expect("history replacement scenario timed out");
}

#[tokio::test]
async fn concurrent_clients_receive_the_same_contiguous_high_contention_history() {
    tokio::time::timeout(Duration::from_secs(15), async {
        const CLIENTS: usize = 4;
        const OPERATIONS_PER_CLIENT: usize = 25;
        let harness = start_harness().await;
        let mut clients = Vec::new();
        for _ in 0..CLIENTS {
            let (mut client, _) = connect_async(&harness.url).await.expect("client connects");
            assert_eq!(caught_up(&mut client).await.1, 0);
            clients.push(client);
        }

        futures_util::future::join_all(clients.iter_mut().enumerate().map(
            |(client_index, client)| async move {
                for operation_index in 0..OPERATIONS_PER_CLIENT {
                    client
                        .send(Message::Binary(
                            vec![0x12, client_index as u8, operation_index as u8].into(),
                        ))
                        .await
                        .expect("publish concurrent operation");
                }
            },
        ))
        .await;

        let expected_count = CLIENTS * OPERATIONS_PER_CLIENT;
        let mut observed_histories = Vec::new();
        for client in &mut clients {
            let mut history = Vec::with_capacity(expected_count);
            for _ in 0..expected_count {
                history.push(decode_position(client.next().await.unwrap().unwrap()));
            }
            observed_histories.push(history);
        }

        for history in &observed_histories[1..] {
            assert_eq!(history, &observed_histories[0]);
        }
        assert_eq!(
            observed_histories[0]
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            (1..=expected_count as u64).collect::<Vec<_>>()
        );
        let history_id = observed_histories[0][0].history_id;
        assert!(observed_histories[0]
            .iter()
            .all(|entry| entry.history_id == history_id));
    })
    .await
    .expect("high-contention collaboration scenario timed out");
}

/// The checkpoint goes to whoever answers the query first, and to exactly one
/// of them. Two clients that both feel able to do the work is the ordinary
/// case, not the exceptional one.
#[tokio::test]
async fn the_first_connection_to_answer_a_checkpoint_query_gets_it_alone() {
    let (redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;
    let state = RedisStateManager::new(pool);
    let room = Uuid::new_v4();
    let _redis = redis;

    assert!(state
        .try_open_reset_query(room)
        .await
        .expect("open checkpoint query"));
    // A second query while one is outstanding is not opened: the room asked.
    assert!(!state
        .try_open_reset_query(room)
        .await
        .expect("query already open"));
    assert!(!state
        .is_reset_uploader(room, "alice")
        .await
        .expect("nobody has answered yet"));

    assert!(state
        .claim_reset_upload(room, "alice")
        .await
        .expect("alice answers first"));
    assert!(!state
        .claim_reset_upload(room, "bob")
        .await
        .expect("bob answers second"));

    assert!(state
        .is_reset_uploader(room, "alice")
        .await
        .expect("selected uploader"));
    assert!(!state
        .is_reset_uploader(room, "bob")
        .await
        .expect("different connection"));

    state
        .clear_reset_pending(room)
        .await
        .expect("clear reset lease");
    assert!(!state
        .is_reset_uploader(room, "alice")
        .await
        .expect("expired authority"));
}

/// A room out of history cannot wait for a window to lapse before asking
/// again -- nobody in it can draw until a checkpoint lands.
#[tokio::test]
async fn a_forced_query_reopens_one_that_is_already_outstanding() {
    let (redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;
    let state = RedisStateManager::new(pool);
    let room = Uuid::new_v4();
    let _redis = redis;

    assert!(state
        .try_open_reset_query(room)
        .await
        .expect("open checkpoint query"));
    assert!(state
        .claim_reset_upload(room, "alice")
        .await
        .expect("alice takes it"));

    assert!(state
        .reopen_reset_query(room)
        .await
        .expect("reopen the query"));

    // Alice's claim is gone, and anyone may now take it -- including Alice, if
    // she is still the only one able.
    assert!(!state
        .is_reset_uploader(room, "alice")
        .await
        .expect("claim released"));

    // Every message a full room refuses arrives here, from every client in it.
    // The second one must not put the room's question to it again.
    assert!(!state
        .reopen_reset_query(room)
        .await
        .expect("the query is already unanswered"));

    assert!(state
        .claim_reset_upload(room, "bob")
        .await
        .expect("bob answers the new query"));
}

#[test]
fn reset_checkpoint_requires_one_snapshot_for_each_layer_of_each_participant() {
    // [type][author][target owner][layer]. One connection uploads the whole
    // canvas, so the author byte is the same throughout and the owner byte is
    // what makes each pair a different participant's.
    assert!(valid_reset_payloads(&[
        vec![0x02, 1, 1, 1, 0xaa],
        vec![0x02, 1, 1, 0, 0xbb],
    ]));
    // Two participants, both pairs complete
    assert!(valid_reset_payloads(&[
        vec![0x02, 1, 1, 1, 0xaa],
        vec![0x02, 1, 1, 0, 0xbb],
        vec![0x02, 1, 7, 1, 0xcc],
        vec![0x02, 1, 7, 0, 0xdd],
    ]));
    // A participant with only one of their two layers
    assert!(!valid_reset_payloads(&[
        vec![0x02, 1, 1, 1, 0xaa],
        vec![0x02, 1, 1, 0, 0xbb],
        vec![0x02, 1, 7, 1, 0xcc],
        vec![0x02, 1, 7, 1, 0xdd],
    ]));
    assert!(!valid_reset_payloads(&[]));
    assert!(!valid_reset_payloads(&[vec![0x02, 1, 1, 1]]));
    // The same layer twice for one participant
    assert!(!valid_reset_payloads(&[
        vec![0x02, 1, 1, 1],
        vec![0x02, 1, 1, 1],
    ]));
    // Not a snapshot
    assert!(!valid_reset_payloads(&[
        vec![0x02, 1, 1, 0],
        vec![0x12, 1, 1, 1],
    ]));
    // Not a layer
    assert!(!valid_reset_payloads(&[
        vec![0x02, 1, 1, 0],
        vec![0x02, 1, 1, 2],
    ]));
}

#[tokio::test]
async fn touching_live_history_refreshes_every_canonical_key_lifetime() {
    let (redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;
    let store = RedisMessageStore::new(pool.clone());
    let room = Uuid::new_v4();
    let _redis = redis;
    let channel = format!("oeee:pubsub:{room}");
    let payload = vec![0x12, 1, 0x11];
    let envelope = room_message(payload.clone());
    store
        .sequence_and_publish(room, &payload, &envelope.from_connection, &channel)
        .await
        .expect("seed live history");

    let keys = [
        format!("oeee:msg_history:v4:{room}"),
        format!("oeee:msg_seq:v4:{room}"),
        format!("oeee:msg_history_id:v4:{room}"),
    ];
    let mut connection = pool.get().await.expect("Redis connection");
    for key in &keys {
        connection
            .expire::<_, ()>(key, 1)
            .await
            .expect("shorten test TTL");
    }
    store
        .touch_history(room)
        .await
        .expect("refresh live history");
    for key in &keys {
        let ttl: i64 = connection.ttl(key).await.expect("read refreshed TTL");
        assert!(ttl > 3500, "{key} was only refreshed to {ttl} seconds");
    }
}

/// One Redis subscription serves everybody in the room.
///
/// The property that matters is not just that it is shared -- it is that
/// sharing it costs no listener a message. A connection joining late must not
/// take delivery away from the one already there, and the room's subscription
/// must go when the last of them leaves, or a process slowly accumulates a
/// Redis connection per room it has ever seen.
#[tokio::test]
async fn one_room_subscription_feeds_every_connection_in_it() {
    let (_redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;
    let room = Uuid::new_v4();
    let channel = RedisStateManager::new(pool.clone()).get_room_channel(room);

    let fanout = RoomFanout::new(&redis_url);
    let mut first = fanout
        .subscribe(room, &channel)
        .await
        .expect("first connection joins the room");
    let mut second = fanout
        .subscribe(room, &channel)
        .await
        .expect("second connection joins the same room");

    assert_eq!(fanout.subscribed_rooms().await, 1, "one subscription, not two");
    assert_eq!(fanout.listeners_in(room).await, 2);

    let sent = RoomBroadcast {
        from_connection: "alice".to_string(),
        target_connection: None,
        seq: Some(7),
        history_id: Some(Uuid::new_v4()),
        payload: vec![0x12, 1, 0x55, 0x66],
    };
    let mut conn = pool.get().await.expect("Redis connection");
    let _: () = conn
        .publish(&channel, sent.encode())
        .await
        .expect("publish to the room");

    for listener in [&mut first, &mut second] {
        let received = tokio::time::timeout(Duration::from_secs(5), listener.receiver.recv())
            .await
            .expect("a broadcast arrives")
            .expect("the stream is live");
        assert_eq!(received.payload, sent.payload);
        assert_eq!(received.seq, sent.seq);
        assert_eq!(received.history_id, sent.history_id);
        assert_eq!(received.from_connection, sent.from_connection);
    }

    // Decoded once for the whole room: both listeners hold the same value.
    fanout.release(room).await;
    assert_eq!(
        fanout.subscribed_rooms().await,
        1,
        "somebody is still in the room"
    );
    assert_eq!(fanout.listeners_in(room).await, 1);

    fanout.release(room).await;
    assert_eq!(
        fanout.subscribed_rooms().await,
        0,
        "the last one out takes the subscription with them"
    );
}
