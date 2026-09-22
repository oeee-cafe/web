//! The keys a provider signs its ID tokens with, kept between tokens.
//!
//! Apple and Google each publish a JWK set and name, in a token's header, the
//! `kid` of the key in it that signed the token. Fetching the set for every
//! sign-in would be a request to them on a page nobody waits twice for, so it
//! is kept; a token naming a key the set does not have fetches it again, in
//! case it was rotated, but no more often than [`REFRESH_AT_MOST`], so a
//! stream of made-up key ids is not a stream of requests to the provider.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::DecodingKey;

/// How long a key set is kept before being asked for again. Both providers
/// rotate rarely.
const KEYS_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// The soonest a key that is not in the set may fetch it again.
const REFRESH_AT_MOST: Duration = Duration::from_secs(60);

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

struct CachedKeys {
    keys: JwkSet,
    fetched_at: Instant,
}

/// Every provider's keys, by the URL they came from (a test serves its own).
fn cache() -> &'static Mutex<HashMap<String, CachedKeys>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedKeys>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

async fn fetch(keys_url: &str) -> Result<JwkSet> {
    Ok(http()
        .get(keys_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// The key `kid` names at `keys_url`: from the cache while it is fresh and
/// has it, from the provider otherwise. `None` when the provider has no such
/// key.
pub async fn decoding_key(keys_url: &str, kid: &str) -> Result<Option<DecodingKey>> {
    let cached = {
        let cache = cache().lock().unwrap();
        cache.get(keys_url).map(|cached| {
            let age = cached.fetched_at.elapsed();
            (
                cached.keys.find(kid).cloned(),
                age < KEYS_FOR,
                age < REFRESH_AT_MOST,
            )
        })
    };
    match cached {
        Some((Some(jwk), true, _)) => return Ok(Some(DecodingKey::from_jwk(&jwk)?)),
        // Not there, and asked for only a moment ago.
        Some((None, true, true)) => return Ok(None),
        _ => {}
    }

    let keys = fetch(keys_url).await?;
    let jwk = keys.find(kid).cloned();
    cache().lock().unwrap().insert(
        keys_url.to_string(),
        CachedKeys {
            keys,
            fetched_at: Instant::now(),
        },
    );
    jwk.map(|jwk| DecodingKey::from_jwk(&jwk))
        .transpose()
        .map_err(Into::into)
}
