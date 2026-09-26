//! What an open page hears without asking: the bell's number, a comment on
//! the post it shows, a new drawing where it is looking.
//!
//! Each page holds one `EventSource` on `/events` (web::handlers::events),
//! and the server sends down it. Server-sent events, not a WebSocket: all of
//! this goes one way, and everything a reader does is already a request of
//! its own. It is plain HTTP, so the session cookie and the proxy need
//! nothing, and the browser reconnects by itself -- which is what carries a
//! page across a deploy, when the colour it was talking to stops.
//!
//! Both colours are up at once during a deploy, and an event can start on
//! either: a comment posted to the new one concerns a page still connected to
//! the old. So nothing is handed to this process's pages directly. `publish`
//! puts the event on one Redis channel, and every process holds a single
//! subscription to it (`listen`) that hands each event to its own pages
//! through a `broadcast` channel. One Redis connection per process, whatever
//! the number of pages, and none to Postgres.
//!
//! An event says only enough for the page to know what to fetch: an id and a
//! number, never content a reader might not be allowed to see. What the
//! bell looks like is rendered per connection, in its reader's language.

use futures_util::StreamExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::redis::RedisPool;
use crate::web::state::Shutdown;

/// The one channel every process subscribes to.
pub const CHANNEL: &str = "oeee:live";

/// How far a slow page may fall behind before it loses events. A page that
/// lags this far reconnects (web::handlers::events), and a reconnected page
/// starts from what the server renders, so nothing is lost for long.
const LOCAL_QUEUE: usize = 256;

/// The longest wait between attempts to get the subscription back.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveEvent {
    /// The bell's number for one reader changed.
    Unread { user_id: Uuid, count: i64 },
    /// One reader has a new notification: the push's own words, already in
    /// their language, and where tapping it goes.
    Notification {
        user_id: Uuid,
        title: String,
        body: String,
        url: String,
        /// Whether `body` quotes what someone wrote -- a comment, a guestbook
        /// entry, a post's title -- which `title` introduces, rather than
        /// saying what happened in a sentence of its own that `title` only
        /// restates. A page shows the one sentence, or the title and the
        /// quote (live.jinja). Absent from an older release's event.
        #[serde(default)]
        quoted: bool,
    },
    /// A post's comments changed. `by` is whoever changed them, whose own
    /// page has already been answered and need not fetch again.
    Comments { post_id: Uuid, by: Option<Uuid> },
    /// A drawing was published. `recent` says whether it is one Home's recent
    /// feed shows everyone: in no community or a public one, not a reply,
    /// not sensitive.
    Post {
        community_id: Option<Uuid>,
        recent: bool,
        by: Uuid,
    },
}

impl LiveEvent {
    /// The one reader an event is for, or `None` for one any page may hear.
    pub fn recipient(&self) -> Option<Uuid> {
        match self {
            LiveEvent::Unread { user_id, .. } | LiveEvent::Notification { user_id, .. } => {
                Some(*user_id)
            }
            LiveEvent::Comments { .. } | LiveEvent::Post { .. } => None,
        }
    }
}

#[derive(Clone)]
pub struct Live {
    local: broadcast::Sender<Arc<LiveEvent>>,
    /// Where events go to reach every process. `None` delivers within this
    /// one only, for tests and the CLI.
    redis: Option<RedisPool>,
}

impl Live {
    pub fn new(redis: RedisPool) -> Self {
        let (local, _) = broadcast::channel(LOCAL_QUEUE);
        Self {
            local,
            redis: Some(redis),
        }
    }

    /// Events that never leave this process.
    pub fn local() -> Self {
        let (local, _) = broadcast::channel(LOCAL_QUEUE);
        Self { local, redis: None }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>> {
        self.local.subscribe()
    }

    /// Sends an event to every page that should hear it, on any process.
    /// In the background: nothing a request does waits on it, and a Redis
    /// that is down costs the pages a live update, not the request.
    pub fn publish(&self, event: LiveEvent) {
        let Some(pool) = self.redis.clone() else {
            let _ = self.local.send(Arc::new(event));
            return;
        };
        let local = self.local.clone();
        tokio::spawn(async move {
            let payload = match serde_json::to_string(&event) {
                Ok(payload) => payload,
                Err(e) => {
                    tracing::error!("live event did not serialise: {e:?}");
                    return;
                }
            };
            let sent: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
                let mut conn = pool.get().await?;
                let _: usize = conn.publish(CHANNEL, payload).await?;
                Ok(())
            }
            .await;
            if let Err(e) = sent {
                // This process's own pages can still hear it.
                tracing::warn!("live event not published to Redis: {e:?}");
                let _ = local.send(Arc::new(event));
            }
        });
    }

    /// Holds this process's subscription to the channel for as long as it
    /// runs, getting it back whenever Redis restarts or the network blinks,
    /// and lets it go at shutdown.
    pub fn listen(&self, redis_url: &str, shutdown: Shutdown) -> JoinHandle<()> {
        let local = self.local.clone();
        let redis_url = redis_url.to_string();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let heard = tokio::select! {
                    heard = hear(&redis_url, &local) => heard,
                    _ = shutdown.signalled() => return,
                };
                match heard {
                    // It was up, so the next failure starts the wait again.
                    Ok(()) => {
                        backoff = Duration::from_secs(1);
                        tracing::warn!("live events subscription ended; resubscribing");
                    }
                    Err(e) => tracing::warn!(
                        "live events subscription failed, retrying in {backoff:?}: {e:?}"
                    ),
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.signalled() => return,
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        })
    }
}

/// One subscription, until its connection ends.
async fn hear(
    redis_url: &str,
    local: &broadcast::Sender<Arc<LiveEvent>>,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(CHANNEL).await?;
    let mut stream = pubsub.on_message();
    while let Some(message) = stream.next().await {
        let payload: String = match message.get_payload() {
            Ok(payload) => payload,
            Err(e) => {
                tracing::warn!("unreadable live event: {e:?}");
                continue;
            }
        };
        match serde_json::from_str::<LiveEvent>(&payload) {
            // The error is that no page is listening, which is fine.
            Ok(event) => {
                let _ = local.send(Arc::new(event));
            }
            // A newer release's event, during a deploy: the page that wants
            // it is on the colour that knows it.
            Err(e) => tracing::debug!("ignoring live event this release does not know: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_goes_over_the_wire_by_its_type() {
        let post_id = Uuid::nil();
        let event = LiveEvent::Comments { post_id, by: None };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"comments""#), "got {json}");
        assert_eq!(serde_json::from_str::<LiveEvent>(&json).unwrap(), event);
    }

    #[test]
    fn only_a_readers_own_events_name_them() {
        let user_id = Uuid::new_v4();
        assert_eq!(
            LiveEvent::Unread { user_id, count: 2 }.recipient(),
            Some(user_id)
        );
        assert_eq!(
            LiveEvent::Post {
                community_id: None,
                recent: true,
                by: user_id
            }
            .recipient(),
            None
        );
    }

    /// Two processes on one Redis -- the two colours of a deploy -- each
    /// hear what the other publishes.
    #[tokio::test]
    async fn an_event_published_by_one_process_reaches_another() {
        use crate::web::handlers::collaborate::protocol_integration_tests::start_redis;
        let (_redis, url) = start_redis().await;
        let pool = bb8_redis::bb8::Pool::builder()
            .build(bb8_redis::RedisConnectionManager::new(url.clone()).unwrap())
            .await
            .unwrap();
        let (blue, green) = (Live::new(pool.clone()), Live::new(pool));
        let shutdown = Shutdown::new();
        green.listen(&url, shutdown.clone());
        let mut heard = green.subscribe();

        let post_id = Uuid::new_v4();
        let event = LiveEvent::Comments { post_id, by: None };
        // The subscription is in the background; publish until it is up.
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                blue.publish(event.clone());
                if let Ok(Ok(got)) =
                    tokio::time::timeout(Duration::from_millis(100), heard.recv()).await
                {
                    return got;
                }
            }
        })
        .await
        .expect("green hears blue");
        assert_eq!(*got, event);
        shutdown.signal();
    }

    #[tokio::test]
    async fn without_redis_an_event_reaches_this_process() {
        let live = Live::local();
        let mut heard = live.subscribe();
        let user_id = Uuid::new_v4();
        live.publish(LiveEvent::Unread { user_id, count: 1 });
        assert_eq!(
            *heard.recv().await.unwrap(),
            LiveEvent::Unread { user_id, count: 1 }
        );
    }
}
