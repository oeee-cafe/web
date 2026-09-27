//! What an inbox request has to be before activitypub_federation reads it.
//!
//! activitypub_federation's axum inbox reads the whole body whatever its
//! size, and checks the signature without checking that the body is the one
//! the signature's digest describes. So this runs first, on every POST to an
//! inbox: the body is bounded, and the signature has to cover the request
//! target, the host, the date and the digest; the digest has to match the
//! body; the request has to have been signed for this host, within the hour.
//! Checking the signature itself against the sender's key is still the
//! library's.

use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use feder_runtime::verification::{self, Policy};

/// The largest activity an inbox reads. Mastodon's limit is the same order;
/// an activity is a few kilobytes, and one with its object embedded tens.
pub const MAX_BODY: usize = 1024 * 1024;

/// Refuse an inbox POST that is too large or whose signature does not cover
/// what it carries. `domain` is the host requests must have been signed for.
pub async fn check(State(domain): State<String>, request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect();
    let headers: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path(), |pq| pq.as_str());
    let signed = verification::Request {
        method: "POST",
        path_and_query,
        headers: &headers,
        body: &body,
    };
    let hosts = [domain.as_str()];
    let checked = verification::parse(&signed).and_then(|signature| {
        verification::check(
            &signature,
            &signed,
            &Policy::new(&hosts),
            chrono::Utc::now().timestamp(),
        )
    });
    if let Err(rejection) = checked {
        tracing::info!(%rejection, path = %parts.uri.path(), "refused an inbox request");
        return (StatusCode::UNAUTHORIZED, rejection.to_string()).into_response();
    }
    next.run(Request::from_parts(parts, Body::from(body))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::Router;
    use feder_runtime::signature::{sign_request_with_key, PrivateKey};
    use tower::ServiceExt as _;

    const DOMAIN: &str = "oeee.test";

    fn app() -> Router {
        Router::new()
            .route("/ap/inbox", post(|body: String| async move { body }))
            .route_layer(axum::middleware::from_fn_with_state(
                DOMAIN.to_owned(),
                check,
            ))
    }

    fn key() -> PrivateKey {
        let keys = activitypub_federation::http_signatures::generate_actor_keypair().unwrap();
        PrivateKey::from_pem(&keys.private_key).unwrap()
    }

    fn signed(body: &str, sent_body: &str, host: &str) -> Request {
        let signature = sign_request_with_key(
            "post",
            &format!("https://{host}/ap/inbox"),
            body.as_bytes(),
            "https://remote.test/users/a#main-key",
            &key(),
            &[],
        )
        .unwrap();
        Request::post("/ap/inbox")
            .header("host", host)
            .header("date", signature.date)
            .header("digest", signature.digest)
            .header("signature", signature.signature)
            .body(Body::from(sent_body.to_owned()))
            .unwrap()
    }

    async fn status(request: Request) -> StatusCode {
        app().oneshot(request).await.unwrap().status()
    }

    /// The signature is the library's to verify; what is checked here is
    /// that it covers what arrived. A signed body swapped for another no
    /// longer passes, which with activitypub_federation's axum inbox it did.
    #[tokio::test]
    async fn the_body_has_to_be_the_one_signed_for() {
        let activity = r#"{"type":"Like"}"#;
        assert_eq!(
            status(signed(activity, activity, DOMAIN)).await,
            StatusCode::OK
        );
        assert_eq!(
            status(signed(activity, r#"{"type":"Delete"}"#, DOMAIN)).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn an_unsigned_or_misdirected_request_is_refused() {
        let unsigned = Request::post("/ap/inbox")
            .header("host", DOMAIN)
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(status(unsigned).await, StatusCode::UNAUTHORIZED);
        let elsewhere = signed("{}", "{}", "elsewhere.test");
        assert_eq!(status(elsewhere).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_body_too_large_is_refused() {
        let huge = "x".repeat(MAX_BODY + 1);
        assert_eq!(
            status(signed(&huge, &huge, DOMAIN)).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
