//! Making a failed htmx request visible.
//!
//! Every htmx-driven control on the site used to fail silently. htmx does not
//! swap an error response by default, and nothing listens for the event it
//! fires instead, so a `hx-put` that came back 403 left the form sitting there
//! exactly as it was — indistinguishable from a click that never registered.
//!
//! The handlers behind those controls answer with a bare status and no body
//! (`hx_edit_post` returns `StatusCode::FORBIDDEN` and nothing else), and
//! `AppError` answers with JSON, because the same error type serves what the
//! page fetches for itself -- the painter's save, the push token's
//! registration. Neither is something we would want swapped into the page.
//!
//! So this layer translates on the way out: when a request that came from htmx
//! is about to be answered with an error, the body is replaced with a short
//! localised sentence and added to the stack of toasts every page carries, using
//! htmx's own `HX-Retarget`. The JSON body stays for everyone else, since only
//! htmx sends `HX-Request`.
//!
//! Handlers that render their own inline error are left alone — see
//! `delete_account_htmx`, which answers 200 with the message it wants shown
//! next to the password field. A toast is the fallback for the handlers that
//! say nothing, not a replacement for saying something useful.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::models::user::AuthSession;
use crate::web::handlers::{get_bundle, safe_get_message};

/// The stack of toasts `base.jinja` renders (`toasts.jinja`), which we add to.
const TOASTS_TARGET: &str = "#toasts";

/// Replace the body of a failed htmx response with a localised sentence, and
/// retarget it at the page's toasts.
pub async fn error_banner(req: Request, next: Next) -> Response {
    let is_htmx = is_htmx(req.headers());

    // Both are needed after the response comes back, and the request is moved
    // into `next` before then.
    let accept_language = req
        .headers()
        .get(header::ACCEPT_LANGUAGE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static(""));
    // The auth layer wraps this one, so the session is already in the
    // extensions by the time we run and the user's own language choice — which
    // outranks the header — is available without extracting it again.
    let preferred_language = req
        .extensions()
        .get::<AuthSession>()
        .and_then(|session| session.user.as_ref())
        .and_then(|user| user.preferred_language.clone());

    let response = next.run(req).await;

    if !is_htmx || !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }

    let bundle = get_bundle(&accept_language, preferred_language);
    // Two messages, not one per status: the reader can act on "it did not
    // save, try again" and cannot act on which of eleven `AppError` variants
    // produced it. The specific error is still in the log and in Sentry.
    let key = if response.status() == StatusCode::FORBIDDEN
        || response.status() == StatusCode::UNAUTHORIZED
    {
        "htmx-error-forbidden"
    } else if response.status().is_client_error() {
        "htmx-error-request"
    } else {
        "htmx-error-server"
    };

    let (mut parts, _) = response.into_parts();
    let body = notice(
        "error",
        &safe_get_message(&bundle, key),
        &safe_get_message(&bundle, "close"),
    );

    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    // Without both of these htmx would swap the sentence over whatever the
    // control happened to target — the row it was deleting, or the whole body.
    // Added to the stack, not over it: a toast already up says something else.
    parts
        .headers
        .insert("HX-Retarget", HeaderValue::from_static(TOASTS_TARGET));
    parts
        .headers
        .insert("HX-Reswap", HeaderValue::from_static("beforeend"));

    Response::from_parts(parts, Body::from(body))
}

/// A toast, in the design system's markup (ds.css, "Toasts"): a notice of
/// `level` -- `success`, `info`, `warning` or `error`, axum-messages' levels in
/// lower case, as the flash messages in `toasts.jinja` spell them.
/// templates/notice_close.jinja is the same close button as a template.
fn notice(level: &str, message: &str, close: &str) -> String {
    let close = html_escape(close);
    let role = if level == "error" {
        r#" role="alert""#
    } else {
        ""
    };
    format!(
        concat!(
            r#"<div class="ds-notice ds-toast ds-notice-{level} htmx-{level}-message"{role}>"#,
            r#"<span class="ds-notice-body">{message}</span>"#,
            r#"<button class="ds-notice-close" type="button" aria-label="{close}" title="{close}" "#,
            r#"onclick="var n=this.parentNode,l=n.parentNode;l.removeChild(n);if(!l.querySelector('.ds-notice'))l.innerHTML=''">"#,
            r#"<svg viewBox="0 0 12 12" aria-hidden="true">"#,
            r#"<path d="M2.5 2.5l7 7m0-7l-7 7" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />"#,
            r#"</svg></button></div>"#,
        ),
        level = level,
        role = role,
        message = html_escape(message),
        close = close,
    )
}

/// A toast for a handler to put up alongside whatever else its htmx response
/// swaps: an `<hx-partial>` aimed at the stack, so the row it answers for and
/// the sentence about it arrive in one response. What a full page load's flash
/// message is to a redirect, this is to an in-place action.
pub fn toast(level: &str, message: &str, close: &str) -> String {
    format!(
        r#"<hx-partial hx-target="{TOASTS_TARGET}" hx-swap="beforeend">{}</hx-partial>"#,
        notice(level, message, close)
    )
}

/// Whether a request came from htmx, and so can be answered with fragments to
/// swap rather than a page or a redirect.
pub fn is_htmx(headers: &axum::http::HeaderMap) -> bool {
    headers.get("HX-Request") == Some(&HeaderValue::from_static("true"))
}

/// Fluent messages are authored by us, but they interpolate nothing here and
/// escaping them costs nothing, so a toast cannot become an injection point
/// if one ever starts carrying a value.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/forbidden", get(|| async { StatusCode::FORBIDDEN }))
            .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
            .route("/fine", get(|| async { "<p>hello</p>" }))
            .layer(axum::middleware::from_fn(error_banner))
    }

    async fn body_string(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        String::from_utf8(bytes.to_vec()).expect("body is utf-8")
    }

    #[tokio::test]
    async fn an_htmx_error_is_retargeted_at_the_toasts() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/forbidden")
                    .header("HX-Request", "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request completes");

        assert_eq!(
            response.headers().get("HX-Retarget").unwrap(),
            TOASTS_TARGET,
            "the sentence belongs in a toast, not over the control that failed"
        );
        assert_eq!(response.headers().get("HX-Reswap").unwrap(), "beforeend");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "the status stands"
        );
        assert!(body_string(response).await.contains("htmx-error-message"));
    }

    #[test]
    fn a_toast_is_a_partial_added_to_the_stack() {
        let toast = toast("success", "Joined <b>", "Close");
        assert!(toast.starts_with(r##"<hx-partial hx-target="#toasts" hx-swap="beforeend">"##));
        assert!(toast.contains("ds-notice ds-toast ds-notice-success"));
        assert!(
            toast.contains("Joined &lt;b&gt;"),
            "a message is text, not markup"
        );
        assert!(
            !toast.contains("role=\"alert\""),
            "only an error interrupts"
        );
        assert!(super::toast("error", "No", "Close").contains("role=\"alert\""));
    }

    #[tokio::test]
    async fn a_server_error_says_something_different_than_a_refusal() {
        let forbidden = app()
            .oneshot(
                Request::builder()
                    .uri("/forbidden")
                    .header("HX-Request", "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request completes");
        let boom = app()
            .oneshot(
                Request::builder()
                    .uri("/boom")
                    .header("HX-Request", "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request completes");

        assert_ne!(
            body_string(forbidden).await,
            body_string(boom).await,
            "'you may not' and 'we broke' are not the same advice"
        );
    }

    #[tokio::test]
    async fn a_request_that_did_not_come_from_htmx_is_untouched() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/forbidden")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request completes");

        assert!(
            response.headers().get("HX-Retarget").is_none(),
            "a fetch() reads the JSON body and must keep getting it"
        );
        assert_eq!(body_string(response).await, "");
    }

    #[tokio::test]
    async fn a_successful_htmx_response_is_untouched() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/fine")
                    .header("HX-Request", "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("request completes");

        assert!(response.headers().get("HX-Retarget").is_none());
        assert_eq!(body_string(response).await, "<p>hello</p>");
    }
}
