//! Carrying a sign-in from a browser back into an app's web view.
//!
//! The apps are web views, and the session the site knows them by is the web
//! view's cookie. Some sign-ins cannot happen in a web view at all -- Google
//! refuses its own pages there, and Apple has no native sheet on Android --
//! so they have to happen in a browser of the system's, whose cookies are
//! not the web view's. The sign-in then lands in the wrong place.
//!
//! A handoff is the thread between the two:
//!
//! 1. the page in the web view asks the site to start one and gets back an
//!    `id` and a `secret`;
//! 2. the app opens the system browser at `/auth/<provider>?handoff=<id>`,
//!    which signs in there as any browser would -- including making an
//!    account, if that is what it comes to;
//! 3. the site records against the handoff which account that was, and the
//!    browser is told it can go back to the app;
//! 4. the page claims the handoff with the `id` and the `secret`, and the
//!    site signs *that* session in.
//!
//! What keeps this honest:
//!
//! - the `secret` never leaves the app; only the `id` is in the URL the
//!   browser is given. A claim needs both, and the secret is kept only as a
//!   hash, so a look at the store is not enough to claim with.
//! - a handoff answers once. The first successful claim deletes it.
//! - it expires in [`HANDOFF_FOR`] minutes, which is a sign-in's worth of
//!   time and not an afternoon's.
//!
//! One trap on the way in, for whoever wires an app to this. The browser has
//! to be a real browser. On iOS that is already so: `/auth/*` is excluded
//! from the universal links in `well_known.rs`, so Safari keeps it. On
//! Android it is not -- `assetlinks.json` asks for `handle_all_urls` and the
//! manifest claims every oeee.cafe URL with no path filter -- so an
//! `ACTION_VIEW` for the handoff URL is caught by the app itself, opens in
//! the same web view, and hands the sign-in back to exactly the place that
//! could not do it. It has to be launched as a Custom Tab, which goes to a
//! browser package and is not re-routed.
//!
//! What it does *not* defend against, which is worth saying plainly: the
//! `id` is as good as the sign-in it is waiting for. Anyone who learns one
//! that is still pending can sign in *as themselves* in their own browser
//! against it, and the app that started it would then be signed in as them
//! -- a forced login, of the same shape as a stolen OAuth `state`. So the id
//! is 32 random bytes, it is never logged, and it lives for minutes. It is
//! only ever handed from the app to the system browser.

use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;

use crate::redis::RedisPool;

/// How long a handoff waits to be claimed. Long enough to sign in with a
/// provider, including making an account; short enough that an id left
/// somewhere is not a key to anything by the time it is found.
pub const HANDOFF_FOR: u64 = 15 * 60;

const PREFIX: &str = "oeee:handoff:";

/// A sign-in under way in a browser on an app's behalf.
#[derive(Serialize, Deserialize)]
struct Handoff {
    /// The secret's SHA-256, never the secret. A claim hashes what it was
    /// given and compares.
    secret_hash: String,
    /// Where the app wants to end up, if it said.
    next: Option<String>,
    /// Who signed in, once somebody has. `None` while it is still waiting.
    user_id: Option<Uuid>,
}

/// The two halves of a new handoff: the `id` the browser is sent with, and
/// the `secret` the app keeps to claim it.
pub struct Started {
    pub id: String,
    pub secret: String,
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn key(id: &str) -> String {
    format!("{PREFIX}{id}")
}

/// Every call here takes one connection from the pool and does all of its
/// work on it. Taking a second while holding the first is how a pool of two
/// deadlocks against itself.
type Conn<'a> = bb8_redis::bb8::PooledConnection<'a, bb8_redis::RedisConnectionManager>;

async fn connect(pool: &RedisPool) -> Result<Conn<'_>> {
    pool.get().await.map_err(|e| anyhow::anyhow!("{e}"))
}

async fn read(conn: &mut Conn<'_>, id: &str) -> Result<Option<Handoff>> {
    let stored: Option<String> = conn.get(key(id)).await?;
    Ok(stored.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Writes `handoff` back under `id`, keeping what is left of its life rather
/// than starting it again: a handoff is only ever as old as when it began.
async fn write(conn: &mut Conn<'_>, id: &str, handoff: &Handoff, ttl: u64) -> Result<()> {
    let _: () = conn
        .set_ex(key(id), serde_json::to_string(handoff)?, ttl)
        .await?;
    Ok(())
}

/// Starts a handoff, to be claimed with the secret this returns.
pub async fn start(pool: &RedisPool, next: Option<String>) -> Result<Started> {
    let id = random_token();
    let secret = random_token();
    let handoff = Handoff {
        secret_hash: sha256::digest(secret.as_str()),
        next,
        user_id: None,
    };
    write(&mut connect(pool).await?, &id, &handoff, HANDOFF_FOR).await?;
    Ok(Started { id, secret })
}

/// Whether `id` names a handoff still waiting to be signed in for. Asked
/// before sending a browser to a provider, so a made-up id is turned away at
/// the door rather than after somebody has signed in for nothing.
pub async fn is_pending(pool: &RedisPool, id: &str) -> Result<bool> {
    let handoff = read(&mut connect(pool).await?, id).await?;
    Ok(handoff.is_some_and(|h| h.user_id.is_none()))
}

/// Records who signed in. The handoff then waits for the app to claim it.
///
/// Answers `false` when there is no such handoff any more -- it expired
/// while the person was signing in, or it has already been claimed -- which
/// is not an error, only a sign-in that arrived too late to be carried.
pub async fn signed_in(pool: &RedisPool, id: &str, user_id: Uuid) -> Result<bool> {
    let mut conn = connect(pool).await?;
    // Whatever is left of the original life, so signing in does not extend
    // how long an unclaimed handoff lingers.
    let ttl: i64 = conn.ttl(key(id)).await?;
    let Some(mut handoff) = read(&mut conn, id).await? else {
        return Ok(false);
    };
    if handoff.user_id.is_some() {
        return Ok(false);
    }
    handoff.user_id = Some(user_id);
    write(&mut conn, id, &handoff, ttl.max(1) as u64).await?;
    Ok(true)
}

/// What a claim found.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// Signed in, as this account, and going on to `next`. The handoff is
    /// spent: a second claim finds nothing.
    Ready { user_id: Uuid, next: Option<String> },
    /// Nobody has signed in for it yet. Ask again.
    Waiting,
    /// No such handoff, the wrong secret, or one already claimed. The three
    /// are one answer on purpose: an id being probed learns nothing from
    /// which it was.
    Unknown,
}

/// Claims a handoff. A successful claim spends it.
pub async fn claim(pool: &RedisPool, id: &str, secret: &str) -> Result<Claim> {
    let mut conn = connect(pool).await?;
    let Some(handoff) = read(&mut conn, id).await? else {
        return Ok(Claim::Unknown);
    };
    if !secret_matches(&handoff.secret_hash, secret) {
        return Ok(Claim::Unknown);
    }
    let Some(user_id) = handoff.user_id else {
        return Ok(Claim::Waiting);
    };
    // Deleted before it is answered, so two claims racing cannot both be
    // told yes: only the one whose DEL removed the key goes on.
    let removed: i64 = conn.del(key(id)).await?;
    if removed == 0 {
        return Ok(Claim::Unknown);
    }
    Ok(Claim::Ready {
        user_id,
        next: handoff.next,
    })
}

/// Compares in a way that does not say, by how long it took, how much of the
/// secret was right.
fn secret_matches(expected_hash: &str, secret: &str) -> bool {
    let actual = sha256::digest(secret);
    let (a, b) = (expected_hash.as_bytes(), actual.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Forgets a handoff without claiming it: the app gave up, or the browser
/// came back with nothing.
pub async fn forget(pool: &RedisPool, id: &str) -> Result<()> {
    let mut conn = connect(pool).await?;
    let _: () = conn.del(key(id)).await?;
    Ok(())
}

/// Against the Redis `REDIS_URL` names, under keys of their own. Skipped
/// when there is no Redis to reach.
#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> Option<RedisPool> {
        let url = std::env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".to_string());
        let manager = bb8_redis::RedisConnectionManager::new(url).ok()?;
        let pool = bb8_redis::bb8::Pool::builder()
            .max_size(2)
            .connection_timeout(std::time::Duration::from_millis(500))
            .build(manager)
            .await
            .ok()?;
        // Only usable if something answers.
        pool.get().await.ok()?;
        Some(pool)
    }

    #[tokio::test]
    async fn a_handoff_carries_the_sign_in_once() {
        let Some(pool) = pool().await else { return };
        let user = Uuid::new_v4();
        let started = start(&pool, Some("/draw".to_string())).await.unwrap();

        // Waiting until somebody signs in.
        assert!(is_pending(&pool, &started.id).await.unwrap());
        assert_eq!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Waiting
        );

        assert!(signed_in(&pool, &started.id, user).await.unwrap());
        assert!(!is_pending(&pool, &started.id).await.unwrap());
        assert_eq!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Ready {
                user_id: user,
                next: Some("/draw".to_string())
            }
        );
        // Spent.
        assert_eq!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Unknown
        );
    }

    #[tokio::test]
    async fn the_secret_is_what_claims_it() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        signed_in(&pool, &started.id, Uuid::new_v4()).await.unwrap();

        // The id alone does not claim: knowing it is not enough.
        for wrong in ["", "not-the-secret", &random_token()] {
            assert_eq!(
                claim(&pool, &started.id, wrong).await.unwrap(),
                Claim::Unknown,
                "{wrong:?}"
            );
        }
        // And a wrong secret does not spend it.
        assert!(matches!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn an_id_nobody_started_is_not_a_handoff() {
        let Some(pool) = pool().await else { return };
        let made_up = random_token();
        assert!(!is_pending(&pool, &made_up).await.unwrap());
        assert_eq!(
            claim(&pool, &made_up, &random_token()).await.unwrap(),
            Claim::Unknown
        );
        // Signing in for one that was never started carries nothing.
        assert!(!signed_in(&pool, &made_up, Uuid::new_v4()).await.unwrap());
    }

    #[tokio::test]
    async fn only_the_first_sign_in_counts() {
        let Some(pool) = pool().await else { return };
        let first = Uuid::new_v4();
        let started = start(&pool, None).await.unwrap();
        assert!(signed_in(&pool, &started.id, first).await.unwrap());
        // A second browser finishing against the same handoff does not
        // replace who it is waiting to hand over.
        assert!(!signed_in(&pool, &started.id, Uuid::new_v4()).await.unwrap());
        assert_eq!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Ready {
                user_id: first,
                next: None
            }
        );
    }

    #[tokio::test]
    async fn giving_up_forgets_it() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        forget(&pool, &started.id).await.unwrap();
        assert!(!is_pending(&pool, &started.id).await.unwrap());
        assert_eq!(
            claim(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Unknown
        );
    }

    /// Signing in does not give an unclaimed handoff a fresh lease.
    #[tokio::test]
    async fn the_clock_starts_when_the_handoff_does() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        let mut conn = pool.get().await.unwrap();
        // Wind it down to a minute, as if it had been waiting a while.
        let _: () = conn.expire(key(&started.id), 60).await.unwrap();
        signed_in(&pool, &started.id, Uuid::new_v4()).await.unwrap();
        let left: i64 = conn.ttl(key(&started.id)).await.unwrap();
        assert!(left <= 60, "{left} seconds left, expected at most 60");
        forget(&pool, &started.id).await.unwrap();
    }
}
