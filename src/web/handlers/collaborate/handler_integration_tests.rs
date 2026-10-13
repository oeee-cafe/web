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

use super::guest::{Collaborator, FLAG_GUEST, FLAG_SPECTATOR};
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
const PUT_IMAGE: u8 = 0x1d;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Room {
    _redis: RedisProcess,
    server: JoinHandle<()>,
    url: String,
    db: PgPool,
    state: AppState,
    room: Uuid,
    users: Vec<Uuid>,
    /// Guests made for the scenario, taken out again with the room.
    guests: Vec<Uuid>,
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

    /// Connects as any participant id -- an account's or a guest's -- as if
    /// from the address `ip`.
    async fn connect_as(&self, participant: Uuid, ip: &str) -> Socket {
        let (socket, _) = connect_async(format!(
            "{}/ws/{}/{}?ip={}",
            self.url, self.room, participant, ip
        ))
        .await
        .expect("the handler accepts the upgrade");
        socket
    }

    /// Watches the room as `participant`, as `?spectate=1` asks to.
    async fn watch_as(&self, participant: Uuid, ip: &str) -> Socket {
        let (socket, _) = connect_async(format!(
            "{}/ws/{}/{}?ip={}&spectate=1",
            self.url, self.room, participant, ip
        ))
        .await
        .expect("the handler accepts the upgrade");
        socket
    }

    /// A guest called `nickname`, as `POST /collaborate/guest` makes one.
    async fn add_guest(&mut self, nickname: &str) -> Uuid {
        let id = sqlx::query_scalar!(
            "INSERT INTO collaborative_guests (nickname) VALUES ($1) RETURNING id",
            nickname
        )
        .fetch_one(&self.db)
        .await
        .expect("insert guest");
        self.guests.push(id);
        id
    }

    async fn active_participants(&self) -> Vec<Uuid> {
        sqlx::query_scalar!(
            "SELECT participant_id FROM collaborative_sessions_participants WHERE session_id = $1 AND is_active ORDER BY participant_id",
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
        sqlx::query!(
            "DELETE FROM collaborative_sessions WHERE owner_guest_id = ANY($1)",
            &self.guests
        )
        .execute(&self.db)
        .await
        .expect("delete guests' sessions");
        sqlx::query!(
            "DELETE FROM collaborative_guests WHERE id = ANY($1)",
            &self.guests
        )
        .execute(&self.db)
        .await
        .expect("delete guests");
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

/// `websocket_collaborate_handler` without the sign-in: the participant comes
/// from the path instead of the session cookie -- a guest if the id is one,
/// an account otherwise -- and the address from `?ip=` instead of Cloudflare.
/// Everything after the upgrade is the real handler.
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
    let nickname = sqlx::query_scalar!(
        "SELECT nickname FROM collaborative_guests WHERE id = $1",
        user
    )
    .fetch_optional(&state.db_pool)
    .await
    .expect("look up guest");
    let who = match nickname {
        Some(nickname) => Collaborator::Guest { id: user, nickname },
        None => Collaborator::User {
            id: user,
            login_name: format!("tester_{}", &user.to_string()[..8]),
        },
    };
    let ip = query.get("ip").cloned();
    let spectate = query.get("spectate").is_some_and(|value| value == "1");
    ws.on_upgrade(move |socket| handle_socket(socket, room, state, who, ip, spectate, resume))
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
        env: crate::web::templates::Templates::new(minijinja::Environment::new()),
        db_pool: db.clone(),
        redis_pool: pool.clone(),
        redis_state: RedisStateManager::new(pool),
        room_fanout: RoomFanout::new(&redis_url),
        push_service: Arc::new(PushService::disabled(db.clone())),
        live: crate::live::Live::local(),
        shutdown: Shutdown::new(),
        // Nothing here federates; the deliverer is never run, and its table is
        // only created if something queues.
        deliverer: Arc::new(crate::federation::Deliverer::new(
            ojak_postgres::PostgresQueue::new(db.clone()),
            crate::federation::ActorKeys::new(db.clone()),
            ojak::client::Client::new(ojak::client::ClientConfig::default()).expect("client"),
            ojak::deliverer::DelivererConfig::default(),
        )),
        fetcher: Arc::new(ojak::fetch::Fetcher::new(
            ojak::client::Client::new(ojak::client::ClientConfig::default()).expect("client"),
            ojak::sig::Scheme::DraftCavage,
        )),
        inbox_queue: ojak::queue::shared(ojak_postgres::PostgresQueue::new(db.clone())),
        uris: crate::federation::serving::uris_unserved("localhost", db.clone()).expect("uris"),
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
        guests: Vec::new(),
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

/// A flood fill as the ground it covered, carrying `coverage` bytes: the
/// heaviest thing an ordinary stroke of the pen puts into history.
fn put_image(author: u8, x: i16, coverage: usize) -> Vec<u8> {
    let mut frame = vec![PUT_IMAGE, author, author, 0];
    frame.extend_from_slice(&x.to_le_bytes());
    frame.extend_from_slice(&0i16.to_le_bytes());
    frame.extend_from_slice(&1i16.to_le_bytes());
    frame.extend_from_slice(&1i16.to_le_bytes());
    frame.extend_from_slice(&[0, 0, 0, 255]);
    frame.extend_from_slice(&(coverage as u32).to_le_bytes());
    frame.resize(frame.len() + coverage, 0);
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
async fn growth_in_bytes_asks_for_a_checkpoint_which_then_stands_in_for_it() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, first) = opening(&mut alice).await;

        // Small messages are not a reason to checkpoint until there are a
        // great many of them: 500 used to ask, which in a busy room was every
        // minute or two. The request would follow the echo of the message
        // that crossed the line, before the next one is sequenced, so a
        // hundred echoes past where it used to come is far enough.
        for _ in 0..600 {
            send(&mut alice, vec![UNDO_POINT, alice_id]).await;
        }
        let mut echoed = 0;
        while echoed < 600 {
            let frame = next_frame(&mut alice, "the undo points' echoes").await;
            assert_ne!(
                frame[0], RESET_REQUEST,
                "asked for a checkpoint after {echoed} small messages"
            );
            if frame[0] == SEQUENCED && sequenced(&frame).2[0] == UNDO_POINT {
                echoed += 1;
            }
        }

        // Weight is: past four megabytes on top of the checkpoint, which
        // here is none.
        for x in 0..5 {
            send(&mut alice, put_image(alice_id, x, 900 * 1024)).await;
        }
        // The room is asked who can checkpoint it: a query, not yet an order.
        let query = next_of_type(&mut alice, RESET_REQUEST).await;
        assert_eq!(query.len(), 11);
        assert_eq!(query[9], 0, "phase: query");
        // Four and a half megabytes is nowhere near full: a client may
        // decline if a checkpoint would take somebody's undo.
        assert_eq!(query[10], 0, "not urgent");

        // Alice offers, and is told to upload.
        send(&mut alice, vec![RESET_OFFER]).await;
        let order = next_of_type(&mut alice, RESET_REQUEST).await;
        assert_eq!(order[9], 1, "phase: upload");
        assert_eq!(order[10], 0, "not urgent");

        // Her checkpoint: one pair, hers, at the position she has applied.
        send(&mut alice, reset_begin(605, 2)).await;
        send(&mut alice, snapshot(alice_id, alice_id, 0)).await;
        send(&mut alice, snapshot(alice_id, alice_id, 1)).await;
        let point = loop {
            let frame = next_of_type(&mut alice, SEQUENCED).await;
            let (_, seq, payload) = sequenced(&frame);
            if payload[0] == RESET_POINT {
                break (seq, payload.to_vec());
            }
        };
        assert_eq!(point.0, 606, "the point is sequenced after the base");
        assert_eq!(u64_at(&point.1, 1), 605);
        assert_eq!(u16::from_le_bytes([point.1[9], point.1[10]]), 2);

        // A late joiner now gets the checkpoint in place of all of it.
        let mut bob = room.connect(1, None).await;
        let (_, replay) = opening(&mut bob).await;
        assert_eq!(
            replay.history_id, first.history_id,
            "a checkpoint keeps the history's identity"
        );
        assert_eq!(replay.last_seq, 606);
        assert_eq!(
            replay
                .entries
                .iter()
                .map(|(seq, payload)| (*seq, payload[0]))
                .collect::<Vec<_>>(),
            vec![(605, SNAPSHOT), (605, SNAPSHOT), (606, RESET_POINT)]
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

/// Bytes are what a late joiner is sent; messages are what it then applies,
/// and ours are small enough that the bytes alone would let a room reach a
/// hundred thousand of them first.
#[tokio::test]
async fn enough_small_messages_ask_for_a_checkpoint_too() {
    let Some(room) = open_room(4).await else {
        return;
    };
    let threshold = super::websocket::RESET_THRESHOLD_MESSAGES;
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let mut alice = room.connect(0, None).await;
        let (alice_id, _) = opening(&mut alice).await;
        for _ in 0..threshold {
            send(&mut alice, vec![UNDO_POINT, alice_id]).await;
        }
        let mut echoed = 0;
        let query = loop {
            let frame = next_frame(&mut alice, "the query").await;
            if frame[0] == RESET_REQUEST {
                break frame;
            }
            if frame[0] == SEQUENCED && sequenced(&frame).2[0] == UNDO_POINT {
                echoed += 1;
            }
        };
        assert!(
            echoed >= threshold - 1,
            "asked after only {echoed} messages"
        );
        assert_eq!(query[9], 0, "phase: query");
        assert_eq!(query[10], 0, "not urgent: a client may decline");
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

/// The name and flags a JOIN or LEAVE carries, as the room hears them:
/// [type][uuid:16][ts:8][flags][len:2][name].
fn person_in(frame: &[u8]) -> (Uuid, u8, String) {
    let len = u16::from_le_bytes([frame[26], frame[27]]) as usize;
    (
        Uuid::from_slice(&frame[1..17]).unwrap(),
        frame[25],
        String::from_utf8(frame[28..28 + len].to_vec()).unwrap(),
    )
}

/// The people in a LAYERS frame: (id, session id, flags, name).
fn roster_of(frame: &[u8]) -> Vec<(Uuid, u8, u8, String)> {
    assert_eq!(frame[0], LAYERS);
    let count = u16::from_le_bytes([frame[1], frame[2]]) as usize;
    let mut at = 3;
    let mut people = Vec::new();
    for _ in 0..count {
        let id = Uuid::from_slice(&frame[at..at + 16]).unwrap();
        let session_id = frame[at + 16];
        let flags = frame[at + 17];
        let len = u16::from_le_bytes([frame[at + 18], frame[at + 19]]) as usize;
        let name = String::from_utf8(frame[at + 20..at + 20 + len].to_vec()).unwrap();
        people.push((id, session_id, flags, name));
        at += 20 + len + 8;
    }
    assert_eq!(at, frame.len(), "the roster is exactly its entries");
    people
}

/// Somebody without an account takes a seat like anybody else, and the room
/// is told, by the flag beside the name, that the name is a guest's.
#[tokio::test]
async fn a_guest_is_seated_and_the_room_is_told_it_is_a_guest() {
    let Some(mut room) = open_room(4).await else {
        return;
    };
    let guest = room.add_guest("miro").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;

        let mut miro = room.connect_as(guest, "203.0.113.7").await;
        let (miro_id, _) = opening(&mut miro).await;
        send(&mut miro, join_frame(guest)).await;

        let mut joined = None;
        let mut roster = None;
        while joined.is_none() || roster.is_none() {
            let frame = next_frame(&mut alice, "the guest's JOIN and the roster").await;
            match frame[0] {
                JOIN => joined = Some(frame),
                LAYERS if roster_of(&frame).len() == 2 => roster = Some(frame),
                _ => {}
            }
        }
        assert_eq!(
            person_in(&joined.unwrap()),
            (guest, FLAG_GUEST, "miro".to_string())
        );
        let roster = roster_of(&roster.unwrap());
        assert_eq!(roster[0].2, 0, "the owner is an account");
        assert_eq!(roster[1], (guest, miro_id, FLAG_GUEST, "miro".to_string()));

        let mut active = room.active_participants().await;
        active.sort();
        let mut expected = vec![room.users[0], guest];
        expected.sort();
        assert_eq!(active, expected);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// The owner may keep a session for accounts.
#[tokio::test]
async fn a_room_that_does_not_allow_guests_turns_one_away() {
    let Some(mut room) = open_room(4).await else {
        return;
    };
    sqlx::query!(
        "UPDATE collaborative_sessions SET allow_guests = false WHERE id = $1",
        room.room
    )
    .execute(&room.db)
    .await
    .expect("turn guests away");
    let guest = room.add_guest("miro").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut miro = room.connect_as(guest, "203.0.113.7").await;
        let goodbye = next_close(&mut miro).await.expect("a close, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);
        assert!(
            room.active_participants().await.is_empty(),
            "no seat was taken"
        );

        // An account is still let in.
        let mut bob = room.connect(1, None).await;
        opening(&mut bob).await;
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// A guest is a member of nothing, so only a public community's session --
/// or one in no community -- is open to one, whatever the owner allowed.
#[tokio::test]
async fn a_guest_comes_in_only_where_the_community_is_public() {
    for (visibility, admitted) in [("unlisted", false), ("private", false), ("public", true)] {
        let Some(mut room) = open_room_in(4, Some(visibility)).await else {
            return;
        };
        let guest = room.add_guest("miro").await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            let mut miro = room.connect_as(guest, "203.0.113.7").await;
            if admitted {
                opening(&mut miro).await;
            } else {
                let goodbye = next_close(&mut miro).await.expect("a close, not a welcome");
                assert_eq!(u16::from(goodbye.code), 1008, "{visibility}");
            }
        })
        .await;
        room.teardown().await;
        outcome.expect("the scenario finished in time");
    }
}

/// What the ban endpoint does once it has checked the owner: the ban -- with
/// the address, for a guest -- then every socket the person has in the room
/// closed.
async fn ban(room: &Room, participant: Uuid) {
    let ip = if room.guests.contains(&participant) {
        super::websocket::participant_ip(&room.state, room.room, participant).await
    } else {
        None
    };
    assert!(
        super::db::ban_participant(&room.db, room.room, participant, ip.as_deref())
            .await
            .expect("ban"),
        "they had been in the room"
    );
    super::websocket::remove_from_room(&room.state, room.room, participant).await;
}

/// The owner sends a guest out: the socket is closed with a reason the client
/// knows not to come back from, the guest cannot come back, and neither can a
/// new guest made from the same address -- though an account from it can.
#[tokio::test]
async fn a_banned_guest_is_sent_out_and_kept_out() {
    let Some(mut room) = open_room(8).await else {
        return;
    };
    let guest = room.add_guest("miro").await;
    let second_guest = room.add_guest("miro again").await;
    let elsewhere = room.add_guest("someone else").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;
        let mut miro = room.connect_as(guest, "203.0.113.7").await;
        opening(&mut miro).await;

        ban(&room, guest).await;
        let goodbye = loop {
            match next_close(&mut miro).await {
                Some(goodbye) => break goodbye,
                None => continue,
            }
        };
        assert_eq!(u16::from(goodbye.code), 1008);
        assert_eq!(goodbye.reason.as_str(), "removed from session");
        let left = next_of_type(&mut alice, LEAVE).await;
        assert_eq!(person_in(&left).0, guest);

        // What the page asks before it opens a socket: whether to say "you
        // were removed". The guest, by id; a new guest, by address; never an
        // account from that address.
        let banned = |who: Option<Collaborator>, ip: &'static str| {
            let db = room.db.clone();
            let room_id = room.room;
            async move {
                super::db::viewer_is_banned(&db, room_id, who.as_ref(), Some(ip))
                    .await
                    .expect("ban check")
            }
        };
        let as_guest = |id: Uuid| Collaborator::Guest {
            id,
            nickname: "x".to_string(),
        };
        assert!(banned(Some(as_guest(guest)), "198.51.100.1").await);
        assert!(banned(Some(as_guest(second_guest)), "203.0.113.7").await);
        assert!(banned(None, "203.0.113.7").await, "nobody yet, from there");
        assert!(!banned(Some(as_guest(elsewhere)), "198.51.100.1").await);
        let account = Collaborator::User {
            id: room.users[1],
            login_name: "x".to_string(),
        };
        assert!(!banned(Some(account), "203.0.113.7").await);

        let mut again = room.connect_as(guest, "198.51.100.1").await;
        let refused = next_close(&mut again)
            .await
            .expect("a close, not a welcome");
        assert_eq!(u16::from(refused.code), 1008, "banned by id");

        let mut cleared_cookies = room.connect_as(second_guest, "203.0.113.7").await;
        let refused = next_close(&mut cleared_cookies)
            .await
            .expect("a close, not a welcome");
        assert_eq!(u16::from(refused.code), 1008, "banned by address");

        let mut other = room.connect_as(elsewhere, "198.51.100.1").await;
        opening(&mut other).await;
        let mut bob = room.connect_as(room.users[1], "203.0.113.7").await;
        opening(&mut bob).await;
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// An account is banned by its id alone: the household behind its address
/// is not sent out with it.
#[tokio::test]
async fn a_banned_account_is_kept_out_by_its_id() {
    let Some(mut room) = open_room(8).await else {
        return;
    };
    let guest = room.add_guest("neighbour").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bob = room.connect_as(room.users[1], "203.0.113.9").await;
        opening(&mut bob).await;
        ban(&room, room.users[1]).await;
        let mut bob = room.connect_as(room.users[1], "203.0.113.9").await;
        let refused = loop {
            match next_close(&mut bob).await {
                Some(goodbye) => break goodbye,
                None => continue,
            }
        };
        assert_eq!(u16::from(refused.code), 1008);
        let mut neighbour = room.connect_as(guest, "203.0.113.9").await;
        opening(&mut neighbour).await;
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// Signing in hands a guest's seats and sessions to the account: a seat the
/// account did not have becomes its, a seat it had already absorbs the
/// guest's, a session the guest opened is the account's to save, a ban
/// follows them, and the seat in Redis moves so their layers stay theirs.
#[tokio::test]
async fn signing_in_hands_a_guests_rooms_to_the_account() {
    let Some(mut room) = open_room(8).await else {
        return;
    };
    let guest = room.add_guest("miro").await;
    let account = room.users[1];
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut miro = room.connect_as(guest, "203.0.113.7").await;
        let (seat, _) = opening(&mut miro).await;

        let owned: Uuid = sqlx::query_scalar!(
            r#"
            INSERT INTO collaborative_sessions (owner_guest_id, title, width, height, is_public, max_participants)
            VALUES ($1, 'a guest''s room', 64, 48, false, 4) RETURNING id
            "#,
            guest
        )
        .fetch_one(&room.db)
        .await
        .expect("a session the guest opened");
        let banned: Uuid = sqlx::query_scalar!(
            r#"
            INSERT INTO collaborative_sessions (owner_id, title, width, height, is_public, max_participants)
            VALUES ($1, 'a room the guest was banned from', 64, 48, false, 4) RETURNING id
            "#,
            room.users[0]
        )
        .fetch_one(&room.db)
        .await
        .expect("another session");
        sqlx::query!(
            "INSERT INTO collaborative_sessions_participants (session_id, guest_id, is_active) VALUES ($1, $2, false)",
            banned,
            guest
        )
        .execute(&room.db)
        .await
        .expect("a seat there");
        sqlx::query!(
            "INSERT INTO collaborative_session_bans (session_id, participant_id) VALUES ($1, $2)",
            banned,
            guest
        )
        .execute(&room.db)
        .await
        .expect("the ban");

        let open = super::guest::adopt_guest(&room.db, guest, account)
            .await
            .expect("adopted");
        assert!(open.contains(&room.room) && open.contains(&owned) && open.contains(&banned));
        for open_room in open {
            room.state
                .redis_state
                .rename_seat(open_room, guest, account)
                .await
                .expect("seat moved");
        }

        assert!(room.active_participants().await.contains(&account));
        assert!(!room.active_participants().await.contains(&guest));
        let owner = sqlx::query_scalar!(
            "SELECT owner_id FROM collaborative_sessions WHERE id = $1",
            owned
        )
        .fetch_one(&room.db)
        .await
        .expect("owner");
        assert_eq!(owner, Some(account));
        let viewer = Collaborator::User {
            id: account,
            login_name: "adopted".to_string(),
        };
        assert!(
            !super::db::viewer_may_enter(&room.db, banned, Some(&viewer), None)
                .await
                .expect("may enter"),
            "the ban came with them"
        );
        let seats = room.state.redis_state.get_user_ids(room.room).await.expect("seats");
        assert_eq!(seats.get(&account), Some(&seat), "the same layers");
        assert_eq!(seats.get(&guest), None);

        // A second claim of the same guest does nothing.
        assert!(super::guest::adopt_guest(&room.db, guest, room.users[0])
            .await
            .expect("no-op")
            .is_empty());

        sqlx::query!("DELETE FROM collaborative_sessions WHERE id = ANY($1)", &[owned, banned][..])
            .execute(&room.db)
            .await
            .expect("clean up");
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// A guest who signs in -- in another tab, say -- while their room is open
/// in this one. The socket still speaks for the guest, whose id the server
/// no longer answers to, so it is closed with a reason the page answers by
/// coming back as the account; and back, it is in the guest's seat.
#[tokio::test]
async fn signing_in_sends_the_guests_open_room_away_to_come_back_as_the_account() {
    let Some(mut room) = open_room(8).await else {
        return;
    };
    let guest = room.add_guest("miro").await;
    let account = room.users[1];
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut miro = room.connect_as(guest, "203.0.113.7").await;
        let (seat, _) = opening(&mut miro).await;

        let session = tower_sessions::Session::new(
            None,
            Arc::new(tower_sessions::MemoryStore::default()),
            None,
        );
        session
            .insert(super::guest::GUEST_KEY, guest)
            .await
            .expect("the browser session holds the guest");
        super::guest::adopt(&room.state, &session, account).await;

        let goodbye = loop {
            match next_close(&mut miro).await {
                Some(goodbye) => break goodbye,
                None => continue,
            }
        };
        assert_eq!(u16::from(goodbye.code), 4001);
        assert_eq!(goodbye.reason.as_str(), "signed in");

        let mut back = room.connect_as(account, "203.0.113.7").await;
        let (back_seat, _) = opening(&mut back).await;
        assert_eq!(back_seat, seat, "the same layers, under the account");
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// The owner stops letting guests in mid-session: a new guest is turned
/// away, and one already in the room is not shut out by a dropped
/// connection.
#[tokio::test]
async fn closing_the_door_to_guests_keeps_out_new_ones_only() {
    let Some(mut room) = open_room(8).await else {
        return;
    };
    let inside = room.add_guest("inside").await;
    let newcomer = room.add_guest("newcomer").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut first = room.connect_as(inside, "203.0.113.7").await;
        opening(&mut first).await;

        sqlx::query!(
            "UPDATE collaborative_sessions SET allow_guests = false WHERE id = $1",
            room.room
        )
        .execute(&room.db)
        .await
        .expect("close the door");

        first.close(None).await.expect("close");
        let mut again = room.connect_as(inside, "203.0.113.7").await;
        opening(&mut again).await;

        let mut late = room.connect_as(newcomer, "198.51.100.1").await;
        let refused = next_close(&mut late).await.expect("a close, not a welcome");
        assert_eq!(u16::from(refused.code), 1008);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

fn chat_frame(sender: Uuid, message: &str) -> Vec<u8> {
    let mut frame = vec![0x03];
    frame.extend_from_slice(sender.as_bytes());
    frame.extend_from_slice(&1_700_000_000_000u64.to_le_bytes());
    frame.extend_from_slice(&(message.len() as u16).to_le_bytes());
    frame.extend_from_slice(message.as_bytes());
    frame
}

/// Somebody watches a room with no seat left: no seat taken and no id to
/// draw under, their drawing dropped before anybody sees it, their chat
/// heard and marked as a spectator's, and the roster saying they watch.
#[tokio::test]
async fn a_spectator_watches_a_full_room_and_talks_but_cannot_draw() {
    let Some(room) = open_room(1).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;

        let watcher = room.users[1];
        let mut bob = room.watch_as(watcher, "203.0.113.7").await;
        let (bob_id, _) = opening(&mut bob).await;
        assert_eq!(bob_id, 0, "no id to draw under");
        assert_eq!(
            sqlx::query_scalar!(
                "SELECT COUNT(*) AS \"n!\" FROM collaborative_sessions_participants WHERE session_id = $1 AND is_active AND NOT is_spectator",
                room.room
            )
            .fetch_one(&room.db)
            .await
            .expect("count"),
            1,
            "the one seat is still Alice's"
        );

        send(&mut bob, join_frame(watcher)).await;
        let mut joined = None;
        let mut roster = None;
        while joined.is_none() || roster.is_none() {
            let frame = next_frame(&mut alice, "the spectator's JOIN and the roster").await;
            match frame[0] {
                JOIN => joined = Some(frame),
                LAYERS if roster_of(&frame).len() == 2 => roster = Some(frame),
                _ => {}
            }
        }
        assert_eq!(person_in(&joined.unwrap()).1, FLAG_SPECTATOR);
        let roster = roster_of(&roster.unwrap());
        assert_eq!((roster[1].0, roster[1].1, roster[1].2), (watcher, 0, FLAG_SPECTATOR));

        // A fill from the spectator never reaches history; a chat does.
        send(&mut bob, fill(0, 10)).await;
        send(&mut bob, chat_frame(watcher, "lovely")).await;
        let chat = next_of_type(&mut alice, 0x03).await;
        assert_eq!(chat[25], FLAG_SPECTATOR);
        send(&mut alice, fill(1, 12)).await;
        let (_, seq, _) = {
            let frame = next_of_type(&mut alice, SEQUENCED).await;
            let (history, seq, payload) = sequenced(&frame);
            (history, seq, payload.to_vec())
        };
        assert_eq!(seq, 1, "the spectator's fill took no place in history");
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// A spectator moves up to drawing once a seat frees, and the room counts
/// them as a drawer from then on.
#[tokio::test]
async fn a_spectator_takes_a_seat_once_one_frees() {
    let Some(room) = open_room(1).await else {
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut alice = room.connect(0, None).await;
        opening(&mut alice).await;
        let watcher = room.users[1];
        let mut bob = room.watch_as(watcher, "203.0.113.7").await;
        opening(&mut bob).await;

        // Full: asking to draw is refused.
        let mut drawing = room.connect(1, None).await;
        let refused = next_close(&mut drawing).await.expect("a close, not a welcome");
        assert_eq!(u16::from(refused.code), 1008);

        alice.close(None).await.expect("close");
        for _ in 0..50 {
            if room.active_participants().await == vec![watcher] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bob.close(None).await.expect("close");
        let mut drawing = room.connect(1, None).await;
        let (seat, _) = opening(&mut drawing).await;
        assert_ne!(seat, 0, "a seat, and an id to draw under");
        let is_spectator = sqlx::query_scalar!(
            "SELECT is_spectator FROM collaborative_sessions_participants WHERE session_id = $1 AND participant_id = $2",
            room.room,
            watcher
        )
        .fetch_one(&room.db)
        .await
        .expect("row");
        assert!(!is_spectator, "a drawer now, credited as one");
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}

/// The owner's switch: new spectators are turned away, somebody already
/// watching is not shut out by a dropped connection, and a full set of
/// spectators turns the next one away as well.
#[tokio::test]
async fn watching_is_the_owners_to_allow_and_has_a_ceiling() {
    let Some(mut room) = open_room(4).await else {
        return;
    };
    let early = room.add_guest("early").await;
    let late = room.add_guest("late").await;
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let mut first = room.watch_as(early, "203.0.113.7").await;
        opening(&mut first).await;

        sqlx::query!(
            "UPDATE collaborative_sessions SET allow_spectators = false WHERE id = $1",
            room.room
        )
        .execute(&room.db)
        .await
        .expect("stop watching");
        first.close(None).await.expect("close");
        let mut again = room.watch_as(early, "203.0.113.7").await;
        opening(&mut again).await;
        let mut refused = room.watch_as(late, "198.51.100.1").await;
        let goodbye = next_close(&mut refused).await.expect("a close, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);

        // Open again, but every place taken.
        sqlx::query!(
            "UPDATE collaborative_sessions SET allow_spectators = true WHERE id = $1",
            room.room
        )
        .execute(&room.db)
        .await
        .expect("allow watching");
        let mut fillers = Vec::new();
        for i in 0..super::db::MAX_SPECTATORS - 1 {
            fillers.push(room.add_guest(&format!("watcher {i}")).await);
        }
        for filler in &fillers {
            sqlx::query!(
                "INSERT INTO collaborative_sessions_participants (session_id, guest_id, is_active, is_spectator) VALUES ($1, $2, true, true)",
                room.room,
                filler
            )
            .execute(&room.db)
            .await
            .expect("a watcher");
        }
        let mut one_too_many = room.watch_as(late, "198.51.100.1").await;
        let goodbye = next_close(&mut one_too_many).await.expect("a close, not a welcome");
        assert_eq!(u16::from(goodbye.code), 1008);
    })
    .await;
    room.teardown().await;
    outcome.expect("the scenario finished in time");
}
