use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

use super::handlers::collaborate::redis_state::RedisStateManager;
use super::handlers::collaborate::room_fanout::RoomFanout;
use super::templates::Templates;
use crate::live::Live;
use crate::push::PushService;
use crate::redis::RedisPool;
use crate::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    /// The templates, which render only through a supporter lookup
    /// (web::templates).
    pub env: Templates,
    pub db_pool: PgPool,
    pub redis_pool: RedisPool,
    pub redis_state: RedisStateManager,
    /// One Redis subscription per room, shared by that room's connections.
    pub room_fanout: RoomFanout,
    pub push_service: Arc<PushService>,
    /// What open pages hear without asking (crate::live).
    pub live: Live,
    pub shutdown: Shutdown,
}

impl AppState {
    /// Renders a page (web::templates::Templates::render).
    pub async fn render(
        &self,
        name: &str,
        ctx: minijinja::Value,
    ) -> Result<String, minijinja::Error> {
        self.env.render(&self.db_pool, name, ctx).await
    }

    /// Renders one block of a page, for an htmx request that replaces it.
    pub async fn render_block(
        &self,
        name: &str,
        block: &str,
        ctx: minijinja::Value,
    ) -> Result<String, minijinja::Error> {
        self.env.render_block(&self.db_pool, name, block, ctx).await
    }
}

/// Lets in-flight WebSocket sessions notice a redeploy and close cleanly.
///
/// Axum's graceful shutdown does not cover upgraded connections: `on_upgrade`
/// spawns a detached task, and the HTTP connection it came from counts as
/// finished the moment the upgrade happens. So without this a SIGTERM kills
/// every live drawing session mid-await — no close frame for the client, no
/// LEAVE for everyone else, and participant rows left marked active, which
/// holds slots in a full room until the 30-minute sweep.
#[derive(Clone)]
pub struct Shutdown {
    // Holding the sender keeps the channel open for the whole process
    // lifetime, so `signalled()` can never resolve on a dropped sender.
    tx: Arc<watch::Sender<bool>>,
    live_sockets: Arc<AtomicUsize>,
}

impl Shutdown {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            live_sockets: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Tells every live session to wind down. Idempotent.
    pub fn signal(&self) {
        let _ = self.tx.send(true);
    }

    pub fn is_signalled(&self) -> bool {
        *self.tx.borrow()
    }

    /// Resolves once shutdown has been signalled — immediately if it already
    /// has, so a socket opened during shutdown does not wait forever.
    pub async fn signalled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|signalled| *signalled).await;
    }

    /// Counts one live socket for as long as the returned guard is held.
    pub fn track_socket(&self) -> SocketGuard {
        self.live_sockets.fetch_add(1, Ordering::Relaxed);
        SocketGuard {
            live_sockets: Arc::clone(&self.live_sockets),
        }
    }

    pub fn live_socket_count(&self) -> usize {
        self.live_sockets.load(Ordering::Relaxed)
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SocketGuard {
    live_sockets: Arc<AtomicUsize>,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.live_sockets.fetch_sub(1, Ordering::Relaxed);
    }
}
