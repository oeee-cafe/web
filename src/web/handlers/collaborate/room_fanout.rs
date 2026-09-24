//! One Redis subscription per room per process, shared by its connections.
//!
//! Every connection used to open its own Pub/Sub connection to Redis and
//! decode the room's whole stream for itself. Eight people in a room meant
//! Redis delivered every stroke eight times, and this process parsed the same
//! header and the same UUID eight times, to decide seven times over that the
//! message was worth forwarding.
//!
//! So the subscription belongs to the room. One task reads it, decodes each
//! broadcast once, and hands out an `Arc` of it; the connections take it from
//! a `broadcast` channel and decide for themselves what to do with it. The
//! last connection to leave takes the subscription with it.

use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};
use uuid::Uuid;

use super::redis_state::RoomBroadcast;
use axum::body::Bytes;

/// How far behind a connection may fall before it is told to reconnect.
///
/// Matched to the per-connection outgoing queue: a connection that cannot keep
/// up with the fanout could not have kept up with its own socket either, and
/// both failures are answered the same way -- close, and let it resume from
/// the position it last acknowledged.
const FANOUT_QUEUE_LIMIT: usize = 1024;

/// How long a join waits for Redis to accept a subscription.
///
/// A join that cannot hear the room is no use to anybody, so past this it is
/// refused and the client retries, rather than holding a socket open on a
/// handshake that may never finish.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

struct RoomEntry {
    /// Tells this entry apart from one that later replaces it under the same
    /// room. The forwarding task and every listener remember the generation
    /// they belong to, so neither can tear down a successor by mistake.
    generation: u64,
    sender: broadcast::Sender<Arc<Delivery>>,
    /// Connections currently holding a listener. The entry goes when it hits
    /// zero; a room with nobody in it has nothing to deliver.
    listeners: usize,
    task: JoinHandle<()>,
}

type Rooms = Arc<Mutex<HashMap<Uuid, RoomEntry>>>;

/// The rooms this process is subscribed to.
#[derive(Clone)]
pub struct RoomFanout {
    rooms: Rooms,
    redis_url: Arc<str>,
    next_generation: Arc<AtomicU64>,
}

/// A broadcast as every listener in the room receives it.
///
/// The SEQUENCED frame a sequenced message is sent as is the same bytes for
/// everyone in the room, so it is built here, once, rather than by each
/// connection's task as it forwarded the message -- eight copies of a
/// checkpoint-sized PUT_IMAGE for an eight-seat room.
#[derive(Debug)]
pub struct Delivery {
    pub broadcast: RoomBroadcast,
    /// The frame to send, for a message that is part of canonical history.
    pub sequenced: Option<Bytes>,
}

impl Delivery {
    pub fn new(broadcast: RoomBroadcast) -> Self {
        let sequenced = broadcast
            .history_id
            .zip(broadcast.seq)
            .map(|(history_id, seq)| {
                Bytes::from(super::websocket::wrap_sequenced(
                    history_id,
                    seq,
                    &broadcast.payload,
                ))
            });
        Self {
            broadcast,
            sequenced,
        }
    }
}

/// One connection's view of a room's stream.
pub struct RoomListener {
    pub receiver: broadcast::Receiver<Arc<Delivery>>,
    subscription: Subscription,
}

/// What `release` needs to give a listener's share back. Kept apart from the
/// listener because the receiver is usually moved into a task of its own
/// while the share is released from somewhere else.
#[derive(Clone, Copy, Debug)]
pub struct Subscription {
    room_uuid: Uuid,
    generation: u64,
}

impl RoomListener {
    pub fn subscription(&self) -> Subscription {
        self.subscription
    }
}

impl RoomFanout {
    pub fn new(redis_url: &str) -> Self {
        Self {
            rooms: Arc::new(Mutex::new(HashMap::new())),
            redis_url: Arc::from(redis_url),
            next_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Joins a room's stream, subscribing to it first if nobody else has.
    ///
    /// The Redis `SUBSCRIBE` has completed by the time this returns, which is
    /// what lets the caller replay history afterwards without a gap: anything
    /// published from here on is already being buffered for this listener.
    pub async fn subscribe(
        &self,
        room_uuid: Uuid,
        channel: &str,
    ) -> Result<RoomListener, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(listener) = self.join_existing(room_uuid).await {
            return Ok(listener);
        }

        // The handshake happens outside the lock. The map is shared by every
        // room on this process, and holding it across a round trip to a Redis
        // that is slow or gone would stall every join and every leave
        // everywhere, not just in the room being opened.
        let pubsub = tokio::time::timeout(SUBSCRIBE_TIMEOUT, async {
            let client = redis::Client::open(&*self.redis_url)?;
            let mut pubsub = client.get_async_pubsub().await?;
            pubsub.subscribe(channel).await?;
            Ok::<_, redis::RedisError>(pubsub)
        })
        .await
        .map_err(|_| {
            format!("Redis did not accept a subscription within {SUBSCRIBE_TIMEOUT:?}")
        })??;

        let mut rooms = self.rooms.lock().await;

        // Two people opened the room at once and the other got here first.
        // Theirs is kept and this one's connection is dropped, which
        // unsubscribes it. Joining theirs keeps the promise above, because
        // theirs was not put in the map until its own SUBSCRIBE had finished.
        if let Some(entry) = rooms.get_mut(&room_uuid) {
            entry.listeners += 1;
            return Ok(RoomListener {
                receiver: entry.sender.subscribe(),
                subscription: Subscription {
                    room_uuid,
                    generation: entry.generation,
                },
            });
        }

        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = broadcast::channel(FANOUT_QUEUE_LIMIT);
        let publisher = sender.clone();
        let task_rooms = self.rooms.clone();
        let task = tokio::spawn(async move {
            let mut pubsub = pubsub;
            let mut stream = pubsub.on_message();
            while let Some(message) = stream.next().await {
                let payload: Vec<u8> = message.get_payload().unwrap_or_default();
                match RoomBroadcast::decode(&payload) {
                    // The error is that nobody is listening, which is the
                    // normal state between the last leave and the abort below.
                    Some(broadcast) => {
                        let _ = publisher.send(Arc::new(Delivery::new(broadcast)));
                    }
                    None => error!(
                        "Dropping unrecognised Redis broadcast ({} bytes)",
                        payload.len()
                    ),
                }
            }
            drop(stream);

            // The stream only ends when the connection does: Redis restarted,
            // or the network between here and it blinked. Nothing reconnects
            // this subscription, so leaving it in the map would keep the room
            // quietly deaf on this process -- its listeners waiting on a
            // channel nobody feeds, and every new joiner handed the same one.
            // Taking it out drops the last sender with it, which is what tells
            // each listener `Closed`, so their clients reconnect and resume,
            // and whoever joins next opens a subscription that works.
            let mut rooms = task_rooms.lock().await;
            if rooms
                .get(&room_uuid)
                .is_some_and(|entry| entry.generation == generation)
            {
                rooms.remove(&room_uuid);
                warn!(
                    "Lost the Redis subscription for room {}; closing its listeners on this process",
                    room_uuid
                );
            }
        });

        debug!(
            "Subscribed to room {} on channel {} for this process",
            room_uuid, channel
        );
        rooms.insert(
            room_uuid,
            RoomEntry {
                generation,
                sender,
                listeners: 1,
                task,
            },
        );
        Ok(RoomListener {
            receiver,
            subscription: Subscription {
                room_uuid,
                generation,
            },
        })
    }

    async fn join_existing(&self, room_uuid: Uuid) -> Option<RoomListener> {
        let mut rooms = self.rooms.lock().await;
        let entry = rooms.get_mut(&room_uuid)?;
        entry.listeners += 1;
        Some(RoomListener {
            receiver: entry.sender.subscribe(),
            subscription: Subscription {
                room_uuid,
                generation: entry.generation,
            },
        })
    }

    /// Gives up one connection's share of a room's subscription.
    ///
    /// Must be called once per successful `subscribe`. It is not a `Drop`
    /// because dropping cannot await, and the map is behind an async lock so
    /// that the forwarding task can take its own entry out when Redis goes.
    ///
    /// A share in a subscription that has since been lost and replaced is not
    /// a share in its replacement, so it is simply dropped: counting it
    /// against the new entry would unsubscribe a room with people still in it.
    pub async fn release(&self, subscription: Subscription) {
        let Subscription {
            room_uuid,
            generation,
        } = subscription;
        let mut rooms = self.rooms.lock().await;
        let Some(entry) = rooms.get_mut(&room_uuid) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.listeners = entry.listeners.saturating_sub(1);
        if entry.listeners == 0 {
            if let Some(entry) = rooms.remove(&room_uuid) {
                entry.task.abort();
                debug!("Unsubscribed from room {}: nobody left here", room_uuid);
            }
        }
    }

    #[cfg(test)]
    pub async fn subscribed_rooms(&self) -> usize {
        self.rooms.lock().await.len()
    }

    #[cfg(test)]
    pub async fn listeners_in(&self, room_uuid: Uuid) -> usize {
        self.rooms
            .lock()
            .await
            .get(&room_uuid)
            .map_or(0, |entry| entry.listeners)
    }
}
