//! Choosing the site's language, signed in or not.
//!
//! A signed in reader's choice is `users.preferred_language`, which outranks
//! everything. Somebody signed out has no row to keep it in, so the choice is
//! a `lang` cookie, and this layer makes the cookie *be* the browser's
//! `Accept-Language` by the time anything reads it. That is the one place a
//! language is asked for: `get_bundle` is called from dozens of handlers, the
//! error banner and the flash helpers, and each of them reads the header, so
//! rewriting the header reaches all of them without any of them knowing.
//!
//! Signing in keeps the cookie and the account's choice both, so the toolbar
//! sets the two together and the account page does too; otherwise picking
//! "Auto" in one would leave the other still choosing.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    Form,
};
use serde::Deserialize;

use crate::app_error::AppError;
use crate::models::user::{update_user_preferred_language, AuthSession, Language};
use crate::web::state::AppState;

const COOKIE: &str = "lang";

/// A year: this is a preference, not a session.
const MAX_AGE: u32 = 60 * 60 * 24 * 365;

/// The form value the toolbar and the account page send for a language.
/// Anything else, "auto" included, means "whatever the browser asks for".
pub fn parse_language(value: Option<&str>) -> Option<Language> {
    match value {
        Some("ko") => Some(Language::Ko),
        Some("ja") => Some(Language::Ja),
        Some("en") => Some(Language::En),
        Some("zh") => Some(Language::Zh),
        _ => None,
    }
}

fn code(language: &Language) -> &'static str {
    match language {
        Language::Ko => "ko",
        Language::Ja => "ja",
        Language::En => "en",
        Language::Zh => "zh",
    }
}

/// The `lang` cookie's value, when it names a language we have.
fn cookie_language(headers: &HeaderMap) -> Option<&'static str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .and_then(|(_, value)| parse_language(Some(value)))
        .map(|language| code(&language))
}

/// Stand the `lang` cookie in for `Accept-Language`.
pub async fn language_cookie(mut req: Request, next: Next) -> Response {
    if let Some(language) = cookie_language(req.headers()) {
        req.headers_mut()
            .insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static(language));
    }
    next.run(req).await
}

/// The `Set-Cookie` that records `language`, or forgets it for "Auto".
pub fn language_set_cookie(language: Option<&Language>, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = match language {
        Some(language) => format!(
            "{COOKIE}={}; Path=/; Max-Age={MAX_AGE}; SameSite=Lax{secure}",
            code(language)
        ),
        None => format!("{COOKIE}=; Path=/; Max-Age=0; SameSite=Lax{secure}"),
    };
    HeaderValue::from_str(&cookie).expect("the cookie is ASCII we wrote")
}

/// Back to the page the choice was made on. Only its path and query are
/// kept, so this cannot be pointed at another site; and a path that begins
/// `//` would be read by the browser as a host, so that goes home instead.
fn back(headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|referer| url::Url::parse(referer).ok())
        .filter(|url| !url.path().starts_with("//"))
        .map(|url| match url.query() {
            Some(query) => format!("{}?{}", url.path(), query),
            None => url.path().to_string(),
        })
        .unwrap_or_else(|| "/".to_string())
}

#[derive(Deserialize)]
pub struct LanguageForm {
    pub language: Option<String>,
}

/// The toolbar's language choice.
pub async fn set_language(
    auth_session: AuthSession,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LanguageForm>,
) -> Result<Response, AppError> {
    let language = parse_language(form.language.as_deref());

    if let Some(user) = auth_session.user.as_ref() {
        let mut tx = state.db_pool.begin().await?;
        update_user_preferred_language(&mut tx, user.id, language.clone()).await?;
        tx.commit().await?;
    }

    let mut response = Redirect::to(&back(&headers)).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        language_set_cookie(language.as_ref(), state.config.env == "production"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(name: header::HeaderName, value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn the_cookie_is_found_among_others() {
        let headers = with(header::COOKIE, "id=abc; lang=ja; theme=dark");
        assert_eq!(cookie_language(&headers), Some("ja"));
    }

    #[test]
    fn a_language_we_do_not_have_is_no_choice() {
        let headers = with(header::COOKIE, "lang=fr");
        assert_eq!(cookie_language(&headers), None);
        let headers = with(header::COOKIE, "notlang=ko");
        assert_eq!(cookie_language(&headers), None);
    }

    #[test]
    fn back_keeps_the_path_and_query_and_nothing_else() {
        let headers = with(header::REFERER, "https://oeee.cafe/@someone?page=2");
        assert_eq!(back(&headers), "/@someone?page=2");
        let headers = with(header::REFERER, "https://elsewhere.example/phish");
        assert_eq!(back(&headers), "/phish");
    }

    #[test]
    fn back_will_not_leave_the_site() {
        let headers = with(header::REFERER, "https://oeee.cafe//elsewhere.example/");
        assert_eq!(back(&headers), "/");
        let headers = with(header::REFERER, "https://oeee.cafe/\\elsewhere.example/");
        assert_eq!(back(&headers), "/");
        assert_eq!(back(&HeaderMap::new()), "/");
    }
}
