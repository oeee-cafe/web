//! The desktop app's Discord account: trading its tokens, for an app that
//! may not hold the secret (`crate::discord::AppGrant`).
//!
//! Asked by the app itself, from outside any page -- no cookie, no Origin --
//! so nothing here reads a session or answers differently for anyone: what
//! it trades is whatever Discord would trade for the code or token sent,
//! with the secret added, and only a code given to the app's own redirect.
//! It keeps nothing.
//!
//! Being open to anyone, each is a request to Discord in this application's
//! name for whoever sends one, and Discord's rate limits are the
//! application's, shared with every sign-in. So each address gets only so
//! many a minute ([`admitted`]).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::app_error::AppError;
use crate::discord::{self, AppGrant};
use crate::redis::RedisPool;
use crate::web::state::AppState;

/// How many asks an address gets in a [`WINDOW`], both endpoints together.
/// The app asks once when the player connects Discord, once a week or so
/// to refresh, and once to disconnect: a handful a day from one computer.
/// Twenty a minute leaves room for a household or an office behind one
/// address, and for an app retrying through a bad minute, while keeping
/// someone with a script well short of Discord's limit for the application.
const ASKS: i64 = 20;
const WINDOW: u64 = 60;

/// Who is asking, as Cloudflare saw them. The site is reached only through
/// the Cloudflare tunnel and then Caddy, so the connection itself is always
/// from this machine; `CF-Connecting-IP` is set by Cloudflare, over whatever
/// a client sent, and is the only address here worth counting by. Without
/// it -- a development server, reached directly -- everyone is one asker.
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .unwrap_or("unknown")
        .to_string()
}

/// Whether `ip` may ask again this minute: a count per address, made when
/// the first ask of a window arrives and gone when the window is, so a
/// steady trickle under the limit is never turned away.
///
/// Redis being out of reach lets the ask through. This is a fence against
/// somebody leaning on Discord through the site, not a lock on anything,
/// and the app's player should not lose Discord because Redis did.
async fn admitted(pool: &RedisPool, ip: &str) -> bool {
    let key = format!("oeee:discord-app-rate:{ip}");
    let counted: anyhow::Result<i64> = async {
        let mut conn = pool.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        // Made with its expiry if it is not there, then counted: the count
        // keeps the expiry it was made with, so the window does not slide.
        let (_, count): (Option<String>, i64) = redis::pipe()
            .atomic()
            .cmd("SET")
            .arg(&key)
            .arg(0)
            .arg("EX")
            .arg(WINDOW)
            .arg("NX")
            .incr(&key, 1)
            .query_async(&mut *conn)
            .await?;
        Ok(count)
    }
    .await;
    match counted {
        Ok(count) => count <= ASKS,
        Err(error) => {
            tracing::warn!("The Discord app endpoints could not be rate limited: {error:#}");
            true
        }
    }
}

/// Tokens for the app's grant, or 400 when Discord will not give them; 404
/// when the site has no `[discord]` to trade with; 429 when this address
/// has asked too often.
pub async fn app_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(grant): Json<AppGrant>,
) -> Result<Response, AppError> {
    let config = state
        .config
        .discord
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Discord".to_string()))?;
    if !admitted(&state.redis_pool, &client_ip(&headers)).await {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    match discord::app_tokens(config, &grant).await? {
        Some(tokens) => Ok(Json(tokens).into_response()),
        None => Err(AppError::BadRequest(
            "Discord would not trade that".to_string(),
        )),
    }
}

#[derive(Deserialize)]
pub struct Revoke {
    token: String,
}

/// Hands a token back to Discord. 204 whatever Discord says, since the app
/// forgets the token either way; 429 when this address has asked too often.
pub async fn app_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(revoke): Json<Revoke>,
) -> Result<Response, AppError> {
    let config = state
        .config
        .discord
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Discord".to_string()))?;
    if !admitted(&state.redis_pool, &client_ip(&headers)).await {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    if let Err(error) = discord::revoke(config, &revoke.token).await {
        tracing::warn!("Discord could not be asked to revoke the app's token: {error:#}");
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Against the Redis `REDIS_URL` names, under keys of their own. Skipped
/// when there is no Redis to reach.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

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
        pool.get().await.ok()?;
        Some(pool)
    }

    #[test]
    fn the_asker_is_who_cloudflare_says() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("cf-connecting-ip", HeaderValue::from_str(value).unwrap());
            headers
        };
        assert_eq!(client_ip(&with("203.0.113.7")), "203.0.113.7");
        assert_eq!(client_ip(&with(" 2001:db8::1 ")), "2001:db8::1");
        // Not an address is no address, and is not a key of its own to
        // spread asks over.
        assert_eq!(client_ip(&with("203.0.113.7, 10.0.0.1")), "unknown");
        assert_eq!(client_ip(&with("x")), "unknown");
        assert_eq!(client_ip(&HeaderMap::new()), "unknown");
    }

    #[tokio::test]
    async fn an_address_gets_so_many_asks_a_minute() {
        let Some(pool) = pool().await else { return };
        let ip = format!("test-{}", uuid::Uuid::new_v4());
        for ask in 1..=ASKS {
            assert!(admitted(&pool, &ip).await, "ask {ask}");
        }
        assert!(!admitted(&pool, &ip).await);
        // Somebody else is not counted with them.
        assert!(admitted(&pool, &format!("{ip}-other")).await);

        // The count goes with the window it was made in.
        let mut conn = pool.get().await.unwrap();
        let left: i64 = redis::cmd("TTL")
            .arg(format!("oeee:discord-app-rate:{ip}"))
            .query_async(&mut *conn)
            .await
            .unwrap();
        assert!(0 < left && left <= WINDOW as i64, "{left}");
        let _: () = redis::cmd("DEL")
            .arg(format!("oeee:discord-app-rate:{ip}"))
            .arg(format!("oeee:discord-app-rate:{ip}-other"))
            .query_async(&mut *conn)
            .await
            .unwrap();
    }
}
