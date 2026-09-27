//! Outgoing ActivityPub deliveries, through feder.
//!
//! Deliveries used to wait in activitypub_federation's in-memory queue, which
//! a blue/green deploy emptied: whatever was queued or being retried when the
//! old colour stopped was never sent. They now wait in PostgreSQL, in the
//! `feder_queue` table feder-postgres creates, and both colours claim from it
//! while both are up; a claim is a lease, so what a stopped colour was holding
//! is taken again when the lease lapses.
//!
//! activitypub_federation still handles everything that comes in and every
//! fetch. This module is only the sending half, which is where the network
//! was losing work.

use crate::models::actor::Actor;
use feder::client::{Client, ClientConfig};
use feder::deliverer::{DelivererConfig, SenderKeys};
use feder::delivery::{PrivateKey, SenderKey};
use feder::queue::QueueError;
use feder_postgres::PostgresQueue;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The deliverer every part of the site sends through.
pub type Deliverer = feder::deliverer::Deliverer<PostgresQueue, ActorKeys>;

/// Build the deliverer, creating its table if it is not there.
///
/// # Errors
///
/// When the table cannot be created or the HTTP client cannot be built.
pub async fn deliverer(pool: PgPool, domain: &str) -> anyhow::Result<Deliverer> {
    let queue = PostgresQueue::new(pool.clone());
    queue.initialize().await?;
    let client = Client::new(ClientConfig {
        user_agent: format!(
            "oeee.cafe/{} (+https://{domain}/)",
            env!("CARGO_PKG_VERSION")
        ),
        ..ClientConfig::default()
    })?;
    Ok(feder::deliverer::Deliverer::new(
        queue,
        ActorKeys::new(pool),
        client,
        DelivererConfig::default(),
    )
    .on_failure(|failure| {
        tracing::warn!(
            inbox = %failure.inbox,
            sender = %failure.sender,
            status = ?failure.status,
            error = %failure.error,
            "gave up on an ActivityPub delivery"
        );
    }))
}

/// Queue `activity` from `actor` to `inboxes`, leaving out this site's own:
/// a local recipient already has what it would be sent.
///
/// # Errors
///
/// When the queue cannot be written.
pub async fn send(
    deliverer: &Deliverer,
    domain: &str,
    actor: &Actor,
    activity: &serde_json::Value,
    inboxes: Vec<url::Url>,
) -> Result<(), QueueError> {
    deliverer
        .send(actor.iri.as_str(), activity, remote(domain, inboxes))
        .await
}

/// `inboxes` without this site's own.
fn remote(domain: &str, inboxes: Vec<url::Url>) -> Vec<url::Url> {
    inboxes
        .into_iter()
        .filter(|inbox| inbox.host_str() != Some(domain))
        .collect()
}

/// Actors' signing keys, from the `actors` table, parsed once each.
pub struct ActorKeys {
    pool: PgPool,
    parsed: Mutex<HashMap<String, SenderKey>>,
}

impl ActorKeys {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            parsed: Mutex::new(HashMap::new()),
        }
    }
}

impl SenderKeys for ActorKeys {
    async fn key(&self, sender: &str) -> Result<Option<SenderKey>, QueueError> {
        if let Some(key) = self.parsed.lock().expect("key cache").get(sender) {
            return Ok(Some(key.clone()));
        }
        let pem = sqlx::query_scalar!("SELECT private_key_pem FROM actors WHERE iri = $1", sender)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| QueueError(error.to_string()))?
            .flatten();
        let Some(pem) = pem else {
            return Ok(None);
        };
        // A key that does not parse is a broken row, not a passing fault: no
        // retry mends it, so it is no key at all, and the delivery fails.
        let private_key = match PrivateKey::from_pem(&pem) {
            Ok(key) => key,
            Err(error) => {
                tracing::error!(sender, %error, "an actor's private key does not parse");
                return Ok(None);
            }
        };
        let key = SenderKey {
            key_id: format!("{sender}#main-key"),
            private_key: Arc::new(private_key),
        };
        self.parsed
            .lock()
            .expect("key cache")
            .insert(sender.to_owned(), key.clone());
        Ok(Some(key))
    }
}

#[cfg(test)]
mod tests {
    use super::remote;

    #[test]
    fn this_sites_own_inboxes_are_left_out() {
        let inboxes = [
            "https://oeee.cafe/ap/inbox",
            "https://mastodon.example/inbox",
            "https://oeee.cafe/ap/users/1/inbox",
            "https://other.example/users/bob/inbox",
        ]
        .map(|url| url.parse().expect("url"))
        .to_vec();
        let kept: Vec<String> = remote("oeee.cafe", inboxes)
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            kept,
            [
                "https://mastodon.example/inbox",
                "https://other.example/users/bob/inbox"
            ]
        );
    }
}
