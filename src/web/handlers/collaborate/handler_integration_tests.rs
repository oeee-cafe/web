//! The real WebSocket handler, end to end.
//!
//! `protocol_integration_tests` drives the store and the fanout through a
//! connection loop of its own, so `handle_socket` -- the seat in Postgres,
//! the id from WELCOME, the roster, the replay as batches, the echo rules,
//! the checkpoint query and upload, the goodbye codes, the seat given back on
//! close -- only ever ran in production. This puts it behind an axum route
//! the way `websocket_collaborate_handler` does, minus the sign-in, and talks
//! to it over real sockets.
//!
//! Needs `DATABASE_URL` (the tests return early without it, like the model
//! tests) and `redis-server` on the path, or `OEEE_TEST_REDIS_URL`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ws::WebSocketUpgrade, Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use super::messages;
use super::protocol_integration_tests::{redis_pool, start_redis, RedisProcess};
use super::redis_state::RedisStateManager;
use super::room_fanout::RoomFanout;
use super::websocket::handle_socket;
use crate::config::AppConfig;
use crate::push::PushService;
use crate::web::state::{AppState, Shutdown};

const JOIN: u8 = 0x01;
const SNAPSHOT: u8 = 0x02;
const RESET_OFFER: u8 = 0x04;
const REPLAY_START: u8 = 0x05;
const LAYERS: u8 = 0x06;
const END_SESSION: u8 = 0x07;
const LEAVE: u8 = 0x09;
const SEQUENCED: u8 = 0x0a;
const RESET_REQUEST: u8 = 0x0b;
const RESET_BEGIN: u8 = 0x0c;
const RESET_POINT: u8 = 0x0d;
const WELCOME: u8 = 0x0e;
const CAUGHT_UP: u8 = 0x0f;
const REPLAY_BATCH: u8 = 0x10;
const FILL: u8 = 0x12;
const UNDO_POINT: u8 = 0x14;
const MOVE_POINTER: u8 = 0x1c;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Room {
    _redis: RedisProcess,
    server: JoinHandle<()>,
    url: String,
    db: PgPool,
    state: AppState,
    room: Uuid,
    users: Vec<Uuid>,
    community_id: Option<Uuid>,
}

impl Drop for Room {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// The room's URL for one of its users, resuming from a position if given.
impl Room {
    fn url_for(&self, user: usize, resume: Option<(Uuid, u64)>) -> String {
        let mut url = format!("{}/ws/{}/{}", self.url, self.room, self.users[user]);
        if let Some((history_id, after_seq)) = resume {
            url.push_str(&format!("?history_id={history_id}&after_seq={after_seq}"));
        }
        url
    }

    async fn connect(&self, user: usize, resume: Option<(Uuid, u64)>) -> Socket {
        let (socket, _) = connect_async(self.url_for(user, resume))
            .await
            .expect("the handler accepts the upgrade");
        socket
    }

    async fn active_participants(&self) -> Vec<Uuid> {
        sqlx::query_scalar!(
            "SELECT user_id FROM collaborative_sessions_participants WHERE session_id = $1 AND is_active ORDER BY user_id",
            self.room
        )
        .fetch_all(&self.db)
        .await
        .expect("participants")
    }

    /// Takes the rows this test made out of the shared database again.
    async fn teardown(self) {
        sqlx::query!(
            "DELETE FROM collaborative_sessions_participants WHERE session_id = $1",
            self.room
        )
        .execute(&self.db)
        .await
        .expect("delete participants");
        sqlx::query!(
            "DELETE FROM collaborative_sessions WHERE id = $1",
            self.room
        )
        .execute(&self.db)
        .await
        .expect("delete session");
        if let Some(community_id) = self.community_id {
            sqlx::query!("DELETE FROM communities WHERE id = $1", community_id)
                .execute(&self.db)
                .await
                .expect("delete community");
        }
        sqlx::query!("DELETE FROM users WHERE id = ANY($1)", &self.users)
            .execute(&self.db)
            .await
            .expect("delete users");
    }
}

/// `websocket_collaborate_handler` without the sign-in: the user comes from
/// the path instead of the session cookie. Everything after the upgrade is
/// the real handler.
async fn upgrade(
    Path((room, user)): Path<(Uuid, Uuid)>,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    let resume = match (
        query
            .get("history_id")
            .and_then(|id| id.parse::<Uuid>().ok()),
        query
            .get("after_seq")
            .and_then(|seq| seq.parse::<u64>().ok()),
    ) {
        (Some(history_id), Some(after_seq)) => Some((history_id, after_seq)),
        _ => None,
    };
    let login_name = format!("tester_{}", &user.to_string()[..8]);
    ws.on_upgrade(move |socket| handle_socket(socket, room, state, user, login_name, resume))
}

fn test_config(db_url: &str, redis_url: &str) -> AppConfig {
    serde_json::from_value(serde_json::json!({
        "env": "test",
        "base_url": "http://localhost",
        "domain": "localhost",
        "port": 0,
        "db_url": db_url,
        "db_max_connections": 2,
        "db_acquire_timeout": 5,
        "redis_url": redis_url,
        "redis_max_connections": 4,
        "official_account_login_name": "oeee",
        "aws_access_key_id": "",
        "aws_secret_access_key": "",
        "aws_region": "auto",
        "aws_s3_bucket": "",
        "r2_endpoint_url": "http://localhost",
        "r2_public_endpoint_url": "http://localhost",
        "smtp_host": "",
        "smtp_user": "",
        "smtp_password": "",
        "apns_key_id": "",
        "apns_team_id": "",
        "apns_key_path": "",
        "apns_environment": "sandbox",
        "apns_topic": "",
        "fcm_service_account_path": "",
        "fcm_project_id": "",
    }))
    .expect("a config with every required field")
}

/// A room with `seats` seats and two users who may sit in it, behind the real
/// handler. None when there is no database to put them in.
async fn open_room(seats: i32) -> Option<Room> {
    open_room_in(seats, None).await
}

/// The same, with the session opened in a community of the given visibility
/// by its owner, who is made a member of it.
async fn open_room_in(seats: i32, community: Option<&str>) -> Option<Room> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let db = PgPool::connect(&db_url).await.ok()?;
    let (redis, redis_url) = start_redis().await;
    let pool = redis_pool(&redis_url).await;

    let mut users = Vec::new();
    for _ in 0..2 {
        let login = format!("handler_test_{}", &Uuid::new_v4().to_string()[..8]);
        let id: Uuid = sqlx::query_scalar!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
            login
        )
        .fetch_one(&db)
        .await
        .expect("insert user");
        users.push(id);
    }
    let community_id: Option<Uuid> = match community {
        None => None,
        Some(visibility) => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO communities (owner_id, name, slug, description, visibility) \
                 VALUES ($1, 'handler test', $2, '', $3::community_visibility) RETURNING id",
            )
            .bind(users[0])
            .bind(format!("handler-test-{}", &Uuid::new_v4().to_string()[..8]))
            .bind(visibility)
            .fetch_one(&db)
            .await
            .expect("insert community");
            sqlx::query!(
                "INSERT INTO community_members (community_id, user_id, role) VALUES ($1, $2, 'owner')",
                id,
                users[0]
            )
            .execute(&db)
            .await
            .expect("owner membership");
            Some(id)
        }
    };
    let room: Uuid = sqlx::query_scalar!(
        r#"
        INSERT INTO collaborative_sessions (owner_id, title, width, height, is_public, max_participants, community_id)
        VALUES ($1, 'handler test', 64, 48, false, $2, $3) RETURNING id
        "#,
        users[0],
        seats,
        community_id
    )
    .fetch_one(&db)
    .await
    .expect("insert session");

    let state = AppState {
        config: test_config(&db_url, &redis_url),
        env: minijinja::Environment::new(),
        db_pool: db.clone(),
        redis_pool: pool.clone(),
        redis_state: RedisStateManager::new(pool),
        room_fanout: RoomFanout::new(&redis_url),
        push_service: Arc::new(PushService::disabled(db.clone())),
        shutdown: Shutdown::new(),
    };
    let app = Router::new()
        .route("/ws/{room}/{user}", get(upgrade))
        .with_state(state.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    Some(Room {
        _redis: redis,
        server,
        url: format!("ws://{address}"),
        db,
        state,
        room,
        users,
        community_id,
    })
}

/// The next binary frame, whatever it is. `waiting_for` names it in the
/// panic when none comes.
async fn next_frame(socket: &mut Socket, waiting_for: &str) -> Vec<u8> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap_or_else(|_| panic!("no frame within five seconds, waiting for {waiting_for}"))
            .expect("the socket stays open")
            .expect("a readable frame");
        match message {
            Message::Binary(bytes) => return bytes.to_vec(),
            Message::Close(frame) => panic!("the server closed the socket: {frame:?}"),
            _ => continue,
        }
    }
}

/// The next frame of the given type, stepping over whatever else the room is
/// saying meanwhile: a roster, a join, somebody's pointer.
async fn next_of_type(socket: &mut Socket, msg_type: u8) -> Vec<u8> {
    let waiting_for = format!("type {msg_type:#04x}");
    for _ in 0..2000 {
        let frame = next_frame(socket, &waiting_for).await;
        if frame[0] == msg_type {
            return frame;
        }
    }
    panic!("no frame of type {msg_type:#04x} among the next two thousand");
}

/// How the server said goodbye, or None if it sent something else first.
async fn next_close(socket: &mut Socket) -> Option<CloseFrame> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an answer within five seconds")?
            .ok()?;
        match message {
            Message::Close(frame) => return frame,
            Message::Binary(_) => return None,
            _ => continue,
        }
    }
}

async fn send(socket: &mut Socket, frame: Vec<u8>) {
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send");
}

fn join_frame(user: Uuid) -> Vec<u8> {
    let mut frame = vec![JOIN];
    frame.extend_from_slice(user.as_bytes());
    frame.extend_from_slice(&1_700_000_000_000u64.to_le_bytes());
    frame
}

/// A fill, sixteen bytes, claiming to be drawn by `claimed`.
fn fill(claimed: u8, x: i16) -> Vec<u8> {
    let mut frame = vec![FILL, claimed, 0, 0];
    frame.extend_from_slice(&x.to_le_bytes());
    frame.extend_from_slice(&4i16.to_le_bytes());
    frame.extend_from_slice(&[0, 0, 0, 255]);
    frame.extend_from_slice(&[0, 0, 0, 0]);
    frame
}

fn snapshot(author: u8, owner: u8, layer: u8) -> Vec<u8> {
    let png = [0x89, b'P', b'N', b'G', 1, 2, 3, 4];
    let mut frame = vec![SNAPSHOT, author, owner, layer];
    frame.extend_from_slice(&(png.len() as u32).to_le_bytes());
    frame.extend_from_slice(&png);
    frame
}

fn reset_begin(base_seq: u64, count: u16) -> Vec<u8> {
    let mut frame = vec![RESET_BEGIN];
    frame.extend_from_slice(&base_seq.to_le_bytes());
    frame.extend_from_slice(&count.to_le_bytes());
    frame
}

struct Replay {
    history_id: Uuid,
    after_seq: u64,
    last_seq: u64,
    /// Every sequenced message the batches carried, in order.
    entries: Vec<(u64, Vec<u8>)>,
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn inflate_batch(frame: &[u8], history_id: Uuid) -> Vec<(u64, Vec<u8>)> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    assert_eq!(frame[0], REPLAY_BATCH);
    assert_eq!(Uuid::from_slice(&frame[1..17]).unwrap(), history_id);
    let count = u32::from_le_bytes(frame[17..21].try_into().unwrap()) as usize;
    let mut body = Vec::new();
    ZlibDecoder::new(&frame[21..])
        .read_to_end(&mut body)
        .expect("a batch that inflates");
    let mut entries = Vec::new();
    let mut at = 0;
    while at < body.len() {
        let seq = u64_at(&body, at);
        let len = u32::from_le_bytes(body[at + 8..at + 12].try_into().unwrap()) as usize;
        entries.push((seq, body[at + 12..at + 12 + len].to_vec()));
        at += 12 + len;
    }
    assert_eq!(
        entries.len(),
        count,
        "a batch carries as many messages as it says"
    );
    entries
}

/// Reads a connection's opening: WELCOME, then the replay through CAUGHT_UP.
async fn opening(socket: &mut Socket) -> (u8, Replay) {
    let welcome = next_of_type(socket, WELCOME).await;
    let session_id = welcome[1];
    let start = next_of_type(socket, REPLAY_START).await;
    let history_id = Uuid::from_slice(&start[1..17]).unwrap();
    let after_seq = u64_at(&start, 17);
    let mut entries = Vec::new();
    let last_seq = loop {
        let frame = next_frame(socket, "the replay").await;
        match frame[0] {
            REPLAY_BATCH => entries.extend(inflate_batch(&frame, history_id)),
            SEQUENCED => panic!("a replay is batches, never a frame per message"),
            CAUGHT_UP => {
                assert_eq!(Uuid::from_slice(&frame[1..17]).unwrap(), history_id);
                break u64_at(&frame, 17);
            }
            _ => continue,
        }
    };
    (
        session_id,
        Replay {
            history_id,
            after_seq,
            last_seq,
            entries,
        },
    )
}

fn sequenced(frame: &[u8]) -> (Uuid, u64, &[u8]) {
    assert_eq!(frame[0], SEQUENCED);
    (
        Uuid::from_slice(&frame[1..17]).unwrap(),
        u64_at(frame, 17),
        &frame[25..],
    )
}

#[tokio::test]
async fn a_join_is_seated_welcomed_and_echoed_under_its_own_id() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, replay) = opening(&mut alice).await;
        assert_eq!((replay.after_seq, replay.last_seq), (0, 0));
        assert!(replay.entries.is_empty());
        assert_eq!(room.active_participants().await, vec![room.users[0]]);

        let mut bob = room.connect(1, None).await;
        let (bob_id, _) = opening(&mut bob).await;
        assert_ne!(alice_id, bob_id, "two people, two ids");
        send(&mut bob, join_frame(room.users[1])).await;
        // Alice hears Bob join and gets the roster with both of them on it.
        // The roster goes out from the server's side of the join, the JOIN
        // itself from Bob's, so the two are not promised in either order.
        let mut joined = None;
        let mut roster = None;
        while joined.is_none() || roster.is_none() {
            let frame = next_frame(&mut alice, "Bob's JOIN and the roster").await;
            match frame[0] {
                JOIN => joined = Some(frame),
                LAYERS => roster = Some(frame),
                _ => {}
            }
        }
        assert_eq!(
            Uuid::from_slice(&joined.unwrap()[1..17]).unwrap(),
            room.users[1]
        );
        let roster = roster.unwrap();
        assert_eq!(u16::from_le_bytes([roster[1], roster[2]]), 2);

        // A fill claiming somebody else's id goes into history under the
        // sender's own, and comes back to everybody, sender included.
        send(&mut alice, fill(bob_id, 10)).await;
        let (history, seq, payload) = {
            let frame = next_of_type(&mut alice, SEQUENCED).await;
            let (history, seq, payload) = sequenced(&frame);
            (history, seq, payload.to_vec())
        };
        assert_eq!(history, replay.history_id);
        assert_eq!(seq, 1);
        assert_eq!(
            payload[1], alice_id,
            "the author byte is the server's to decide"
        );
        let bobs = next_of_type(&mut bob, SEQUENCED).await;
        assert_eq!(sequenced(&bobs).2, &payload[..]);

        // A pointer reaches the others and never comes back to its sender.
        let mut pointer = vec![MOVE_POINTER, bob_id];
        pointer.extend_from_slice(&40i32.to_le_bytes());
        pointer.extend_from_slice(&80i32.to_le_bytes());
        send(&mut bob, pointer.clone()).await;
        assert_eq!(next_of_type(&mut alice, MOVE_POINTER).await, pointer);
        send(&mut alice, fill(alice_id, 12)).await;
        let next_for_bob = next_frame(&mut bob, "the next fill").await;
        assert_eq!(
            next_for_bob[0], SEQUENCED,
            "Bob's own pointer did not come back before the next fill"
        );

        // Leaving gives the seat back and tells the room.
        bob.close(None).await.expect("close");
        let left = next_of_type(&mut alice, LEAVE).await;
        assert_eq!(Uuid::from_slice(&left[1..17]).unwrap(), room.users[1]);
        for _ in 0..50 {
            if room.active_participants().await == vec![room.users[0]] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(room.active_participants().await, vec![room.users[0]]);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

#[tokio::test]
async fn a_late_joiner_replays_in_batches_and_a_resume_gets_only_what_it_missed() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, first) = opening(&mut alice).await;
        for x in 0..5 {
            send(&mut alice, fill(alice_id, x)).await;
            next_of_type(&mut alice, SEQUENCED).await;
        }

        let mut bob = room.connect(1, None).await;
        let (_, replay) = opening(&mut bob).await;
        assert_eq!(replay.history_id, first.history_id);
        assert_eq!((replay.after_seq, replay.last_seq), (0, 5));
        assert_eq!(
            replay
                .entries
                .iter()
                .map(|(seq, _)| *seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(replay
            .entries
            .iter()
            .all(|(_, payload)| payload[0] == FILL && payload[1] == alice_id));
        bob.close(None).await.expect("close");

        // Back after missing the last two.
        let mut bob = room.connect(1, Some((first.history_id, 3))).await;
        let (_, resumed) = opening(&mut bob).await;
        assert_eq!((resumed.after_seq, resumed.last_seq), (3, 5));
        assert_eq!(
            resumed
                .entries
                .iter()
                .map(|(seq, _)| *seq)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        bob.close(None).await.expect("close");

        // A position on some other history is refused, and everything comes.
        let mut bob = room.connect(1, Some((Uuid::new_v4(), 3))).await;
        let (_, refused) = opening(&mut bob).await;
        assert_eq!((refused.after_seq, refused.last_seq), (0, 5));
        assert_eq!(refused.entries.len(), 5);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

#[tokio::test]
async fn a_full_room_turns_the_next_person_away_with_a_policy_close() {
    let Some(room) = open_room(1).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;
        let mut bob = room.connect(1, None).await;
        let goodbye = next_close(&mut bob)
            .await
            .expect("a close frame, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);
        assert_eq!(
            room.active_participants().await,
            vec![room.users[0]],
            "no seat was taken"
        );
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

#[tokio::test]
async fn five_hundred_messages_ask_for_a_checkpoint_which_then_stands_in_for_them() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, first) = opening(&mut alice).await;
        for _ in 0..500 {
            send(&mut alice, vec![UNDO_POINT, alice_id]).await;
        }
        // The room is asked who can checkpoint it: a query, not yet an order.
        let query = next_of_type(&mut alice, RESET_REQUEST).await;
        assert_eq!(query.len(), 10);
        assert_eq!(query[9], 0, "phase: query");

        // Alice offers, and is told to upload.
        send(&mut alice, vec![RESET_OFFER]).await;
        let order = next_of_type(&mut alice, RESET_REQUEST).await;
        assert_eq!(order[9], 1, "phase: upload");

        // Her checkpoint: one pair, hers, at the position she has applied.
        send(&mut alice, reset_begin(500, 2)).await;
        send(&mut alice, snapshot(alice_id, alice_id, 0)).await;
        send(&mut alice, snapshot(alice_id, alice_id, 1)).await;
        let point = loop {
            let frame = next_of_type(&mut alice, SEQUENCED).await;
            let (_, seq, payload) = sequenced(&frame);
            if payload[0] == RESET_POINT {
                break (seq, payload.to_vec());
            }
        };
        assert_eq!(point.0, 501, "the point is sequenced after the base");
        assert_eq!(u64_at(&point.1, 1), 500);
        assert_eq!(u16::from_le_bytes([point.1[9], point.1[10]]), 2);

        // A late joiner now gets the checkpoint in place of the five hundred.
        let mut bob = room.connect(1, None).await;
        let (_, replay) = opening(&mut bob).await;
        assert_eq!(
            replay.history_id, first.history_id,
            "a checkpoint keeps the history's identity"
        );
        assert_eq!(replay.last_seq, 501);
        assert_eq!(
            replay
                .entries
                .iter()
                .map(|(seq, payload)| (*seq, payload[0]))
                .collect::<Vec<_>>(),
            vec![(500, SNAPSHOT), (500, SNAPSHOT), (501, RESET_POINT)]
        );
        assert_eq!(
            replay.entries[0].1[2], alice_id,
            "whose pair the snapshots are"
        );
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

#[tokio::test]
async fn a_checkpoint_from_somebody_not_asked_is_counted_off_the_wire_and_dropped() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, _) = opening(&mut alice).await;
        let mut bob = room.connect(1, None).await;
        let (bob_id, _) = opening(&mut bob).await;

        // Nobody asked Bob. His snapshots must not enter history as ordinary
        // messages, which would stamp his canvas over everybody's.
        send(&mut bob, reset_begin(0, 2)).await;
        send(&mut bob, snapshot(bob_id, bob_id, 0)).await;
        send(&mut bob, snapshot(bob_id, bob_id, 1)).await;
        send(&mut alice, fill(alice_id, 1)).await;
        let frame = next_of_type(&mut alice, SEQUENCED).await;
        let (_, seq, payload) = sequenced(&frame);
        assert_eq!(
            (seq, payload[0]),
            (1, FILL),
            "the fill is the first thing in history"
        );
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// What a save does to the room, run the way the save runs it: without the
/// owner's socket having to say anything.
#[tokio::test]
async fn finishing_a_session_sends_everyone_to_the_post_and_closes_the_room() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;
        let mut bob = room.connect(1, None).await;
        opening(&mut bob).await;

        messages::finish_session(
            &room.state,
            room.room,
            room.users[0],
            "/@owner/post",
            "system",
        )
        .await;

        for socket in [&mut alice, &mut bob] {
            let frame = next_of_type(socket, END_SESSION).await;
            let len = u16::from_le_bytes([frame[17], frame[18]]) as usize;
            assert_eq!(&frame[19..19 + len], b"/@owner/post");
        }
        let ended: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar!(
            "SELECT ended_at FROM collaborative_sessions WHERE id = $1",
            room.room
        )
        .fetch_one(&room.db)
        .await
        .expect("session row");
        assert!(ended.is_some(), "ended in the database");

        // And the room is closed to the next person.
        let mut late = room.connect(1, None).await;
        let goodbye = next_close(&mut late).await.expect("a close, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// A session in a private community is for the community's members, as its
/// posts are. The lobby and the preview already hid it from everyone else;
/// the join took anyone with the link.
#[tokio::test]
async fn a_room_in_a_private_community_admits_members_only() {
    let Some(room) = open_room_in(4, Some("private")).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        // The owner, a member by construction.
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;

        // Bob holds the link and nothing else.
        let mut bob = room.connect(1, None).await;
        let goodbye = next_close(&mut bob).await.expect("a close, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);
        assert_eq!(
            room.active_participants().await,
            vec![room.users[0]],
            "no seat was taken"
        );

        // Made a member, he is let in.
        sqlx::query!(
            "INSERT INTO community_members (community_id, user_id) VALUES ($1, $2)",
            room.community_id.unwrap(),
            room.users[1]
        )
        .execute(&room.db)
        .await
        .expect("membership");
        let mut bob = room.connect(1, None).await;
        opening(&mut bob).await;
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// An unlisted community is reachable by link, as its pages are.
#[tokio::test]
async fn a_room_in_an_unlisted_community_admits_the_link() {
    let Some(room) = open_room_in(4, Some("unlisted")).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bob = room.connect(1, None).await;
        opening(&mut bob).await;
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}
