//! Error pages for a browser, in place of `AppError`'s JSON.
//!
//! `AppError` answers with JSON because the same type serves what the painter
//! and the apps fetch. A browser loading a page that failed got that JSON
//! too — `{"code":"NOT_FOUND",...}` in place of the site — so handlers took to
//! drawing their own 404 and 403 pages before returning. This layer does it
//! once, on the way out: a response carrying [`ErrorPage`] (every `AppError`'s,
//! and nothing else) becomes the page for its status when the request came
//! from a browser navigating, and stays JSON for everyone else.
//!
//! An `Unauthorized` for somebody signed out is a redirect to the sign-in
//! page instead, coming back afterwards to where they were.
//!
//! htmx requests are `web::htmx::error_banner`'s, which turns them into a
//! toast; this layer leaves them alone.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};

use crate::app_error::{AppError, ErrorPage};
use crate::models::user::{AuthSession, User};
use crate::web::context::CommonContext;
use crate::web::handlers::auth::login_redirect;
use crate::web::htmx::is_htmx;
use crate::web::i18n::ftl_lang;
use crate::web::state::AppState;

pub async fn error_pages(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let wants_page = wants_page(req.uri().path(), req.headers());
    // Read before the request is moved into `next`. The auth layer wraps this
    // one, so the session is already in the extensions.
    let user = req
        .extensions()
        .get::<AuthSession>()
        .and_then(|session| session.user.clone());
    let accept_language = req
        .headers()
        .get(header::ACCEPT_LANGUAGE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static(""));
    // For a sign-in redirect, which comes back to the page the request was on.
    let (method, uri) = (req.method().clone(), req.uri().clone());
    let referer = req.headers().get(header::REFERER).cloned();

    let response = next.run(req).await;

    if !wants_page || response.extensions().get::<ErrorPage>().is_none() {
        return response;
    }
    let status = response.status();
    // Somebody signed out is asked to sign in, as `require_login` asks them,
    // rather than told they may not.
    if status == StatusCode::UNAUTHORIZED && user.is_none() {
        let mut headers = HeaderMap::new();
        if let Some(referer) = referer {
            headers.insert(header::REFERER, referer);
        }
        return login_redirect(&method, &uri, &headers);
    }
    let Some(template) = template_for(status) else {
        return response;
    };

    let ftl_lang = ftl_lang(
        &accept_language,
        user.as_ref().and_then(|u| u.preferred_language.clone()),
    );
    match render(&state, template, user.as_ref(), ftl_lang).await {
        Ok(page) => (status, Html(page)).into_response(),
        // The page is a courtesy; failing to draw it is no reason to lose the
        // status and message the handler meant to send.
        Err(err) => {
            tracing::error!("failed to render {template}: {err}");
            response
        }
    }
}

/// A browser navigating asks for HTML by name; `fetch()` asks for `*/*`, and
/// `/api/` is JSON whoever asks.
fn wants_page(path: &str, headers: &HeaderMap) -> bool {
    !path.starts_with("/api/")
        && !is_htmx(headers)
        && headers
            .get(header::ACCEPT)
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"))
}

fn template_for(status: StatusCode) -> Option<&'static str> {
    match status {
        StatusCode::NOT_FOUND => Some("404.jinja"),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Some("403.jinja"),
        status if status.is_server_error() => Some("500.jinja"),
        _ => None,
    }
}

async fn render(
    state: &AppState,
    template: &str,
    user: Option<&User>,
    ftl_lang: String,
) -> Result<String, AppError> {
    // The header counts only exist for signed-in users, and a 404 absorbs
    // every bot scan for /wp-admin and friends — so don't open a transaction
    // there is nothing to ask.
    let common = match user {
        Some(user) => {
            let mut tx = state.db_pool.begin().await?;
            let common = CommonContext::build(&mut tx, Some(user), &ftl_lang).await?;
            tx.commit().await?;
            common
        }
        None => CommonContext::anonymous(&ftl_lang),
    };
    Ok(state
        .render_page(template, common, minijinja::context! {})
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn a_browser_navigating_gets_a_page() {
        let firefox = headers(&[(
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )]);
        assert!(wants_page("/@someone/missing", &firefox));
    }

    #[test]
    fn fetch_htmx_and_the_api_keep_their_json() {
        assert!(!wants_page("/posts/x", &headers(&[("accept", "*/*")])));
        assert!(!wants_page("/posts/x", &headers(&[])));
        assert!(!wants_page(
            "/posts/x",
            &headers(&[("accept", "text/html"), ("hx-request", "true")])
        ));
        assert!(!wants_page(
            "/api/posts/x",
            &headers(&[("accept", "text/html")])
        ));
    }

    #[test]
    fn only_the_statuses_with_a_page_are_replaced() {
        assert_eq!(template_for(StatusCode::NOT_FOUND), Some("404.jinja"));
        assert_eq!(template_for(StatusCode::FORBIDDEN), Some("403.jinja"));
        assert_eq!(template_for(StatusCode::UNAUTHORIZED), Some("403.jinja"));
        assert_eq!(
            template_for(StatusCode::INTERNAL_SERVER_ERROR),
            Some("500.jinja")
        );
        // A form that failed validation says why in its JSON; a page saying
        // only "bad request" would say less.
        assert_eq!(template_for(StatusCode::BAD_REQUEST), None);
    }
}
