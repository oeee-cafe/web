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
//! 3. the site records against the handoff *who the provider said that was*
//!    -- not an account here -- and the browser is told to go back;
//! 4. the page claims the handoff with the `id` and the `secret`, and the
//!    site does with that identity, in the app's own session, exactly what
//!    it would have done had the sign-in happened there: sign in, link it
//!    to whoever is already signed in, or ask for a username.
//!
//! The direction matters. The browser signs nobody in and touches no
//! account: it is only a messenger for what the provider said. Carrying an
//! account *into* the browser instead -- so it could link on the app's
//! behalf -- would mean anyone who learned a pending id could attach their
//! own provider account to somebody else's, which is a silent and permanent
//! way in. This way the only session that ever changes is the one holding
//! the secret.
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

use crate::models::identity::VerifiedIdentity;
use crate::redis::RedisPool;
use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

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
    /// What the provider said, once it has said it. `None` while the
    /// browser is still out there.
    identity: Option<VerifiedIdentity>,
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
        identity: None,
    };
    write(&mut connect(pool).await?, &id, &handoff, HANDOFF_FOR).await?;
    Ok(Started { id, secret })
}

/// Whether `id` names a handoff still waiting to be signed in for. Asked
/// before sending a browser to a provider, so a made-up id is turned away at
/// the door rather than after somebody has signed in for nothing.
pub async fn is_pending(pool: &RedisPool, id: &str) -> Result<bool> {
    let handoff = read(&mut connect(pool).await?, id).await?;
    Ok(handoff.is_some_and(|h| h.identity.is_none()))
}

/// Records what the provider said. The handoff then waits for the app.
///
/// Answers `false` when there is no such handoff any more -- it expired
/// while the person was signing in, or it has already been claimed -- which
/// is not an error, only a sign-in that arrived too late to be carried.
pub async fn verified(pool: &RedisPool, id: &str, identity: &VerifiedIdentity) -> Result<bool> {
    let mut conn = connect(pool).await?;
    // Whatever is left of the original life, so signing in does not extend
    // how long an unclaimed handoff lingers.
    let ttl: i64 = conn.ttl(key(id)).await?;
    let Some(mut handoff) = read(&mut conn, id).await? else {
        return Ok(false);
    };
    if handoff.identity.is_some() {
        return Ok(false);
    }
    handoff.identity = Some(identity.clone());
    write(&mut conn, id, &handoff, ttl.max(1) as u64).await?;
    Ok(true)
}

/// What a look at a handoff found.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// The provider has said who this is, and the app may act on it.
    Ready {
        identity: Box<VerifiedIdentity>,
        next: Option<String>,
    },
    /// Nobody has finished signing in for it yet. Ask again.
    Waiting,
    /// No such handoff, the wrong secret, or one already spent. The three
    /// are one answer on purpose: an id being probed learns nothing from
    /// which it was.
    Unknown,
}

/// Looks at a handoff without spending it, so the app can show what it is
/// about to do -- which provider account, and whose -- before doing it.
/// Linking one account to another is not something to do behind somebody's
/// back on the strength of an id alone.
pub async fn peek(pool: &RedisPool, id: &str, secret: &str) -> Result<Claim> {
    let Some(handoff) = read(&mut connect(pool).await?, id).await? else {
        return Ok(Claim::Unknown);
    };
    if !secret_matches(&handoff.secret_hash, secret) {
        return Ok(Claim::Unknown);
    }
    match handoff.identity {
        None => Ok(Claim::Waiting),
        Some(identity) => Ok(Claim::Ready {
            identity: Box::new(identity),
            next: handoff.next,
        }),
    }
}

/// Spends a handoff, having acted on it. Answers whether this call was the
/// one that spent it: two claims racing cannot both be told yes, because
/// only the one whose DEL removed the key gets `true`.
pub async fn spend(pool: &RedisPool, id: &str, secret: &str) -> Result<bool> {
    let mut conn = connect(pool).await?;
    let Some(handoff) = read(&mut conn, id).await? else {
        return Ok(false);
    };
    if !secret_matches(&handoff.secret_hash, secret) {
        return Ok(false);
    }
    let removed: i64 = conn.del(key(id)).await?;
    Ok(removed == 1)
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

const AT_PROVIDER_PREFIX: &str = "oeee:handoff-at-provider:";

/// A sign-in the browser was sent straight to the provider for, rather than
/// to `/auth/<provider>?handoff=<id>` first: which handoff it is for, and the
/// nonce the provider's answer has to carry.
///
/// The iOS and macOS apps open the browser in `ASWebAuthenticationSession`,
/// which first asks whether the app may "use" the first page's domain to sign
/// in. Sent to this site first, that named oeee.cafe under a Sign in with
/// Google button; sent to Google, it names Google. The Windows app is sent
/// straight there too, which saves its browser the stop here. The cost is
/// that the browser's session cookie cannot carry the handoff and the nonce
/// to the callback, since the browser never visits this site before Google,
/// so they are kept here under the OAuth `state` instead. The `state` is as good as
/// the handoff's id -- it is in the URL the browser is given -- and gets the
/// same treatment: random, never logged, used once, and gone when the
/// handoff is.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtProvider {
    pub id: String,
    pub nonce: String,
}

fn at_provider_key(state: &str) -> String {
    format!("{AT_PROVIDER_PREFIX}{state}")
}

/// Remembers that the provider's answer carrying `state` is for handoff `id`.
pub async fn send_to_provider(pool: &RedisPool, state: &str, request: &AtProvider) -> Result<()> {
    let mut conn = connect(pool).await?;
    let _: () = conn
        .set_ex(
            at_provider_key(state),
            serde_json::to_string(request)?,
            HANDOFF_FOR,
        )
        .await?;
    Ok(())
}

/// What `state` was sent to the provider for, once: taken as it is read, so
/// an answer replayed at the callback finds nothing.
pub async fn back_from_provider(pool: &RedisPool, state: &str) -> Result<Option<AtProvider>> {
    let mut conn = connect(pool).await?;
    let key = at_provider_key(state);
    let (stored, _): (Option<String>, i64) = redis::pipe()
        .atomic()
        .get(&key)
        .del(&key)
        .query_async(&mut *conn)
        .await?;
    Ok(stored.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Against the Redis `REDIS_URL` names, under keys of their own. Skipped
/// when there is no Redis to reach.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::identity::Provider;

    async fn pool() -> Option<RedisPool> {
        let url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
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

    fn identity(subject: &str) -> VerifiedIdentity {
        VerifiedIdentity {
            provider: Provider::Google,
            subject: subject.to_string(),
            name: Some("오이".to_string()),
            email: Some("oeee@example.test".to_string()),
            purchased: None,
        }
    }

    fn subject_of(claim: &Claim) -> Option<&str> {
        match claim {
            Claim::Ready { identity, .. } => Some(identity.subject.as_str()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_handoff_carries_what_the_provider_said_once() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, Some("/draw".to_string())).await.unwrap();

        // Waiting until the browser gets somewhere.
        assert!(is_pending(&pool, &started.id).await.unwrap());
        assert_eq!(
            peek(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Waiting
        );

        assert!(verified(&pool, &started.id, &identity("g-1"))
            .await
            .unwrap());
        assert!(!is_pending(&pool, &started.id).await.unwrap());

        // A peek says what it is without using it up, so the app can ask
        // before it links anything.
        let looked = peek(&pool, &started.id, &started.secret).await.unwrap();
        assert_eq!(subject_of(&looked), Some("g-1"));
        assert!(matches!(&looked, Claim::Ready { next, .. } if next.as_deref() == Some("/draw")));
        assert_eq!(
            subject_of(&peek(&pool, &started.id, &started.secret).await.unwrap()),
            Some("g-1"),
            "a peek must not spend it"
        );

        assert!(spend(&pool, &started.id, &started.secret).await.unwrap());
        // Spent: only the first spend wins, which is what stops one
        // sign-in being used twice.
        assert!(!spend(&pool, &started.id, &started.secret).await.unwrap());
        assert_eq!(
            peek(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Unknown
        );
    }

    /// The whole thread, end to end, without a browser: start a handoff,
    /// have a provider sign-in record an identity against it, and claim it.
    #[tokio::test]
    async fn a_handoff_carries_an_identity_from_one_place_to_another() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, Some("/draw".to_string())).await.unwrap();

        // Nothing to take yet.
        assert_eq!(
            peek(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Waiting
        );
        // The browser arrives at /auth/google?handoff=<id> and is let in.
        assert!(is_pending(&pool, &started.id).await.unwrap());

        // Google says who it is; the browser hands that over and stops.
        let said = identity("110169484474386276334");
        assert!(verified(&pool, &started.id, &said).await.unwrap());

        // The app looks before it acts, and what it sees is what the
        // provider said -- not an account, which is the point.
        match peek(&pool, &started.id, &started.secret).await.unwrap() {
            Claim::Ready { identity, next } => {
                assert_eq!(*identity, said);
                assert_eq!(next.as_deref(), Some("/draw"));
            }
            other => panic!("expected the identity, got {other:?}"),
        }

        // Acting on it spends it, once.
        assert!(spend(&pool, &started.id, &started.secret).await.unwrap());
        assert_eq!(
            peek(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Unknown
        );
    }

    #[tokio::test]
    async fn the_secret_is_what_claims_it() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        verified(&pool, &started.id, &identity("g-2"))
            .await
            .unwrap();

        // The id alone does not claim: knowing it is not enough, which is
        // the whole reason the secret stays in the app.
        for wrong in ["", "not-the-secret", &random_token()] {
            assert_eq!(
                peek(&pool, &started.id, wrong).await.unwrap(),
                Claim::Unknown,
                "{wrong:?}"
            );
            assert!(
                !spend(&pool, &started.id, wrong).await.unwrap(),
                "{wrong:?}"
            );
        }
        // And none of that spent it.
        assert_eq!(
            subject_of(&peek(&pool, &started.id, &started.secret).await.unwrap()),
            Some("g-2")
        );
        forget(&pool, &started.id).await.unwrap();
    }

    #[tokio::test]
    async fn an_id_nobody_started_is_not_a_handoff() {
        let Some(pool) = pool().await else { return };
        let made_up = random_token();
        assert!(!is_pending(&pool, &made_up).await.unwrap());
        assert_eq!(
            peek(&pool, &made_up, &random_token()).await.unwrap(),
            Claim::Unknown
        );
        // Finishing a sign-in for one that was never started carries nothing.
        assert!(!verified(&pool, &made_up, &identity("g-3")).await.unwrap());
    }

    #[tokio::test]
    async fn only_the_first_answer_counts() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        assert!(verified(&pool, &started.id, &identity("first"))
            .await
            .unwrap());
        // A second browser finishing against the same handoff does not
        // replace the identity waiting to be handed over.
        assert!(!verified(&pool, &started.id, &identity("second"))
            .await
            .unwrap());
        assert_eq!(
            subject_of(&peek(&pool, &started.id, &started.secret).await.unwrap()),
            Some("first")
        );
        forget(&pool, &started.id).await.unwrap();
    }

    #[tokio::test]
    async fn giving_up_forgets_it() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        forget(&pool, &started.id).await.unwrap();
        assert!(!is_pending(&pool, &started.id).await.unwrap());
        assert_eq!(
            peek(&pool, &started.id, &started.secret).await.unwrap(),
            Claim::Unknown
        );
    }

    /// Finishing the sign-in does not give an unclaimed handoff a fresh lease.
    #[tokio::test]
    async fn the_clock_starts_when_the_handoff_does() {
        let Some(pool) = pool().await else { return };
        let started = start(&pool, None).await.unwrap();
        let mut conn = pool.get().await.unwrap();
        // Wind it down to a minute, as if it had been waiting a while.
        let _: () = conn.expire(key(&started.id), 60).await.unwrap();
        drop(conn);
        verified(&pool, &started.id, &identity("g-4"))
            .await
            .unwrap();
        let mut conn = pool.get().await.unwrap();
        let left: i64 = conn.ttl(key(&started.id)).await.unwrap();
        assert!(left <= 60, "{left} seconds left, expected at most 60");
        drop(conn);
        forget(&pool, &started.id).await.unwrap();
    }

    #[tokio::test]
    async fn a_state_sent_to_the_provider_answers_once() {
        let Some(pool) = pool().await else { return };
        let state = random_token();
        let request = AtProvider {
            id: random_token(),
            nonce: random_token(),
        };
        send_to_provider(&pool, &state, &request).await.unwrap();
        assert_eq!(
            back_from_provider(&pool, &state).await.unwrap(),
            Some(request)
        );
        assert_eq!(back_from_provider(&pool, &state).await.unwrap(), None);
        assert_eq!(
            back_from_provider(&pool, &random_token()).await.unwrap(),
            None
        );
    }
}
