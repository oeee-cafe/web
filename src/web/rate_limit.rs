//! Counting asks by address, for the few things anybody may do without an
//! account and somebody with a script could do a great deal of.

use axum::http::HeaderMap;

use crate::redis::RedisPool;

/// Who is asking, as Cloudflare saw them. The site is reached only through
/// the Cloudflare tunnel and then Caddy, so the connection itself is always
/// from this machine; `CF-Connecting-IP` is set by Cloudflare, over whatever
/// a client sent, and is the only address here worth counting by. None
/// without it -- a development server, reached directly -- or when it is not
/// one address.
pub fn client_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_string)
}

/// Whether the asker counted under `key` may ask again in this window: a
/// count made when the first ask of a window arrives and gone when the
/// window is, so a steady trickle under `limit` is never turned away.
///
/// Redis being out of reach lets the ask through. These are fences against
/// somebody leaning on the site, not locks on anything, and nobody should
/// lose what they were doing because Redis did.
pub async fn admitted(pool: &RedisPool, key: &str, limit: i64, window_secs: u64) -> bool {
    let counted: anyhow::Result<i64> = async {
        let mut conn = pool.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        // Made with its expiry if it is not there, then counted: the count
        // keeps the expiry it was made with, so the window does not slide.
        let (_, count): (Option<String>, i64) = redis::pipe()
            .atomic()
            .cmd("SET")
            .arg(key)
            .arg(0)
            .arg("EX")
            .arg(window_secs)
            .arg("NX")
            .incr(key, 1)
            .query_async(&mut *conn)
            .await?;
        Ok(count)
    }
    .await;
    match counted {
        Ok(count) => count <= limit,
        Err(error) => {
            tracing::warn!("{key} could not be rate limited: {error:#}");
            true
        }
    }
}
