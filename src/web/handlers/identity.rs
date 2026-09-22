//! Signing in with another service's account: Steam now, Microsoft and Apple
//! after it.
//!
//! A provider's own handler turns what its client sent into a
//! [`VerifiedIdentity`] and hands it to [`sign_in_with`], which is the same
//! for all of them:
//!
//! - linked to an account: sign into it (or, already signed into that
//!   account, carry on);
//! - signed into another account: link to that one, since the person asked
//!   from inside it;
//! - an email address the provider vouches for, matching an account's
//!   verified address: link to that account and sign into it;
//! - otherwise: keep the identity in the session and ask for a username to
//!   make an account with -- or for a sign-in to an existing account, which
//!   is then linked.
//!
//! The session cookie is SameSite=Lax, so a form another site submits
//! arrives signed out and cannot link anything to the reader's account. That
//! is not enough here on its own: arriving signed out, it would start a new
//! session -- replacing the reader's cookie -- holding the other site's Steam
//! account, and the reader's next password sign-in would link it. So the
//! POSTs that take an identity also have to come from this site's own pages
//! ([`from_this_site`]). The Steam app's post does: it is made from the page.

use axum::extract::{Path, Query, State};
use axum::http::header::ORIGIN;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use axum_messages::Messages;
use chrono::{DateTime, Duration, Utc};
use fluent::bundle::FluentBundle;
use fluent::{FluentArgs, FluentResource, FluentValue};
use intl_memoizer::concurrent::IntlLangMemoizer;
use minijinja::context;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::app_error::AppError;
use crate::models::identity::{
    find_user_by_identity, find_user_by_verified_email, link_identity, touch_identity,
    unlink_identity, LinkError, Provider, UnlinkError, VerifiedIdentity,
};
use crate::models::user::{
    create_user, find_user_by_login_name, login_name_conflicts_with_community,
    update_user_email_verified_at, update_user_preferred_language, AuthSession, User, UserDraft,
};
use crate::steam::{self, TicketRejected};
use crate::web::handlers::{
    detect_preferred_language, get_bundle, safe_format_message, safe_get_message,
    ExtractAcceptLanguage, ExtractFtlLang,
};
use crate::web::state::AppState;

type Bundle<'a> = FluentBundle<&'a FluentResource, IntlLangMemoizer>;

const PENDING_KEY: &str = "identity.pending";

/// How long a verified identity waits in the session for its account to be
/// chosen. Long enough to read the guidelines; short enough that a session
/// left open somewhere does not hold one indefinitely.
const PENDING_FOR: i64 = 30;

/// An identity a provider has vouched for, not yet linked to an account.
#[derive(Clone, Serialize, Deserialize)]
struct PendingIdentity {
    identity: VerifiedIdentity,
    next: Option<String>,
    verified_at: DateTime<Utc>,
}

async fn pending(session: &Session) -> Option<PendingIdentity> {
    let pending: PendingIdentity = session.get(PENDING_KEY).await.ok().flatten()?;
    if Utc::now() - pending.verified_at > Duration::minutes(PENDING_FOR) {
        let _ = session.remove::<PendingIdentity>(PENDING_KEY).await;
        return None;
    }
    Some(pending)
}

/// The name of the provider waiting to be linked, for the sign-in page to say
/// what signing in will do.
pub async fn pending_provider_name(session: &Session) -> Option<&'static str> {
    pending(session)
        .await
        .map(|pending| pending.identity.provider.display_name())
}

/// A place on this site to go on to, and nowhere else: a path, not a URL, and
/// not `//host`, which a browser reads as one.
fn local_next(next: Option<&str>) -> Option<String> {
    let next = next?.trim();
    let local = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.chars().any(char::is_control);
    local.then(|| next.to_string())
}

/// Whether a POST came from one of this site's own pages, by its `Origin`.
/// A request without one is turned away: every browser these forms are
/// posted from -- the Steam app's webview, and whatever a new account is
/// made in right after -- sends it.
fn from_this_site(headers: &HeaderMap, base_url: &str) -> bool {
    let Some(origin) = headers.get(ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let (Ok(origin), Ok(site)) = (url::Url::parse(origin), url::Url::parse(base_url)) else {
        return false;
    };
    origin.origin() == site.origin()
}

fn provider_args(provider: Provider) -> FluentArgs<'static> {
    let mut args = FluentArgs::new();
    args.set("provider", FluentValue::from(provider.display_name()));
    args
}

fn say(bundle: &Bundle<'_>, key: &str, provider: Provider) -> String {
    safe_format_message(bundle, key, Some(&provider_args(provider)))
}

fn welcome(messages: &Messages, bundle: &Bundle<'_>, user: &User) {
    let mut args = FluentArgs::new();
    args.set("name", FluentValue::from(user.display_name.clone()));
    messages
        .clone()
        .success(safe_format_message(bundle, "welcome", Some(&args)));
}

fn bundle_for<'a>(accept_language: &'a HeaderValue, user: Option<&User>) -> Bundle<'a> {
    get_bundle(
        accept_language,
        user.and_then(|user| user.preferred_language.clone()),
    )
}

/// Everything after a provider has said who someone is. See the module
/// documentation for the order things are tried in.
async fn sign_in_with(
    auth_session: &mut AuthSession,
    session: &Session,
    messages: &Messages,
    bundle: &Bundle<'_>,
    state: &AppState,
    identity: VerifiedIdentity,
    next: Option<String>,
) -> Result<Response, AppError> {
    let provider = identity.provider;
    let mut tx = state.db_pool.begin().await?;
    let linked = find_user_by_identity(&mut tx, provider, &identity.subject).await?;
    let current = auth_session.user.clone();

    match (linked, current) {
        (Some(linked), None) => {
            touch_identity(&mut tx, &identity).await?;
            tx.commit().await?;
            auth_session
                .login(&linked)
                .await
                .map_err(|_| AppError::Unauthorized)?;
            welcome(messages, bundle, &linked);
            Ok(Redirect::to(next.as_deref().unwrap_or("/")).into_response())
        }
        (Some(linked), Some(current)) if linked.id == current.id => {
            touch_identity(&mut tx, &identity).await?;
            tx.commit().await?;
            Ok(Redirect::to(next.as_deref().unwrap_or("/")).into_response())
        }
        (Some(_), Some(_)) => {
            // Signed in as one account and holding the key to another. Which
            // one was meant is not ours to guess; switching would surprise
            // whoever is at the keyboard more than staying does.
            messages
                .clone()
                .error(say(bundle, "identity-signed-in-as-other", provider));
            Ok(Redirect::to(next.as_deref().unwrap_or("/account")).into_response())
        }
        (None, Some(current)) => {
            let key = match link_identity(&mut tx, current.id, &identity).await? {
                Ok(()) => "identity-linked",
                Err(LinkError::LinkedElsewhere) => "identity-linked-elsewhere",
                Err(LinkError::ProviderAlreadyLinked) => "identity-provider-already-linked",
            };
            tx.commit().await?;
            if key == "identity-linked" {
                messages.clone().success(say(bundle, key, provider));
            } else {
                messages.clone().error(say(bundle, key, provider));
            }
            Ok(Redirect::to(next.as_deref().unwrap_or("/account")).into_response())
        }
        (None, None) => {
            // The provider vouches for the address, and the account proved it
            // owns it: the same person.
            if let Some(email) = identity.email.as_deref() {
                if let Some(user) = find_user_by_verified_email(&mut tx, email).await? {
                    if link_identity(&mut tx, user.id, &identity).await?.is_ok() {
                        tx.commit().await?;
                        auth_session
                            .login(&user)
                            .await
                            .map_err(|_| AppError::Unauthorized)?;
                        messages
                            .clone()
                            .success(say(bundle, "identity-linked", provider));
                        welcome(messages, bundle, &user);
                        return Ok(Redirect::to(next.as_deref().unwrap_or("/")).into_response());
                    }
                }
            }
            drop(tx);

            session
                .insert(
                    PENDING_KEY,
                    PendingIdentity {
                        identity,
                        next,
                        verified_at: Utc::now(),
                    },
                )
                .await
                .map_err(|e| AppError::InvalidFormData(e.to_string()))?;
            Ok(Redirect::to("/auth/welcome").into_response())
        }
    }
}

/// Links the identity waiting in the session to `user`, who has just signed
/// in or signed up to claim it. Called by the password sign-in and sign-up
/// handlers; does nothing when no identity is waiting.
pub async fn link_pending_identity(
    session: &Session,
    state: &AppState,
    messages: &Messages,
    bundle: &Bundle<'_>,
    user: &User,
) -> Result<(), AppError> {
    let Some(pending) = pending(session).await else {
        return Ok(());
    };
    let _ = session.remove::<PendingIdentity>(PENDING_KEY).await;

    let provider = pending.identity.provider;
    let mut tx = state.db_pool.begin().await?;
    let key = match link_identity(&mut tx, user.id, &pending.identity).await? {
        Ok(()) => "identity-linked",
        Err(LinkError::LinkedElsewhere) => "identity-linked-elsewhere",
        Err(LinkError::ProviderAlreadyLinked) => "identity-provider-already-linked",
    };
    tx.commit().await?;
    if key == "identity-linked" {
        messages.clone().success(say(bundle, key, provider));
    } else {
        messages.clone().error(say(bundle, key, provider));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct NextQuery {
    next: Option<String>,
}

/// `/auth/steam/app` is a link only the Oeee Cafe app on Steam can follow:
/// the app stops the navigation, asks Steam for a ticket and posts it to
/// `/auth/steam`. A browser that follows it lands here instead.
pub async fn steam_app_only(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    Query(query): Query<NextQuery>,
) -> impl IntoResponse {
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    messages
        .clone()
        .info(safe_get_message(&bundle, "steam-sign-in-app-only"));
    let back = if auth_session.user.is_some() {
        "/account".to_string()
    } else {
        match local_next(query.next.as_deref()) {
            Some(next) => format!("/login?next={}", urlencoding::encode(&next)),
            None => "/login".to_string(),
        }
    };
    Redirect::to(&back)
}

#[derive(Deserialize)]
pub struct SteamSignInForm {
    /// A Web API ticket from `GetAuthTicketForWebApi`, hex-encoded.
    ticket: String,
    next: Option<String>,
}

pub async fn do_steam_sign_in(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SteamSignInForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    let next = local_next(form.next.as_deref());
    let back = if auth_session.user.is_some() {
        "/account"
    } else {
        "/login"
    };

    let Some(config) = state.config.steam.as_ref() else {
        messages
            .clone()
            .error(safe_get_message(&bundle, "steam-sign-in-unavailable"));
        return Ok(Redirect::to(back).into_response());
    };

    let identity = match steam::verify_ticket(config, &form.ticket).await {
        Ok(Ok(identity)) => identity,
        Ok(Err(TicketRejected::Invalid)) => {
            messages
                .clone()
                .error(say(&bundle, "identity-sign-in-invalid", Provider::Steam));
            return Ok(Redirect::to(back).into_response());
        }
        Ok(Err(TicketRejected::Banned)) => {
            messages
                .clone()
                .error(safe_get_message(&bundle, "steam-sign-in-banned"));
            return Ok(Redirect::to(back).into_response());
        }
        Err(error) => {
            tracing::error!("Steam sign-in could not be checked: {error:#}");
            messages
                .clone()
                .error(say(&bundle, "identity-sign-in-failed", Provider::Steam));
            return Ok(Redirect::to(back).into_response());
        }
    };

    sign_in_with(
        &mut auth_session,
        &session,
        &messages,
        &bundle,
        &state,
        identity,
        next,
    )
    .await
}

/// A handle as the `users` table's own constraint allows it:
/// `^[a-zA-Z0-9_-]{1,50}$`. Written out rather than asked of the database so
/// the page can say what is wrong instead of failing the insert.
fn valid_login_name(login_name: &str) -> bool {
    !login_name.is_empty()
        && login_name.len() <= 50
        && login_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A handle to start from, made from what the provider calls the person:
/// the characters a handle may use, in order. Often nothing is left -- a
/// Korean or Japanese name has none of them -- and the field starts empty.
fn suggested_login_name(name: Option<&str>) -> String {
    name.unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(50)
        .collect()
}

fn welcome_page(
    state: &AppState,
    messages: Messages,
    ftl_lang: String,
    pending: &PendingIdentity,
    login_name: &str,
    display_name: &str,
    error: Option<String>,
) -> Result<Html<String>, AppError> {
    let template = state.env.get_template("identity_welcome.jinja")?;
    let rendered = template.render(context! {
        messages => messages.into_iter().collect::<Vec<_>>(),
        ftl_lang,
        provider => pending.identity.provider.display_name(),
        provider_name => pending.identity.name,
        login_name,
        display_name,
        error,
        next => pending.next,
    })?;
    Ok(Html(rendered))
}

/// Choosing a username for an account made by signing in with a provider.
pub async fn identity_welcome(
    auth_session: AuthSession,
    session: Session,
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    if auth_session.user.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let Some(pending) = pending(&session).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    let name = pending.identity.name.clone().unwrap_or_default();
    let login_name = suggested_login_name(pending.identity.name.as_deref());
    Ok(welcome_page(
        &state,
        messages,
        ftl_lang,
        &pending,
        &login_name,
        &name,
        None,
    )?
    .into_response())
}

#[derive(Deserialize)]
pub struct WelcomeForm {
    login_name: String,
    display_name: String,
    agree: Option<String>,
}

pub async fn do_identity_welcome(
    mut auth_session: AuthSession,
    session: Session,
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<WelcomeForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    if auth_session.user.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let Some(pending) = pending(&session).await else {
        return Ok(Redirect::to("/login").into_response());
    };
    let bundle = bundle_for(&accept_language, None);
    let login_name = form.login_name.trim().to_string();
    let display_name = form.display_name.trim().to_string();

    let mut tx = state.db_pool.begin().await?;
    let problem = if form.agree.is_none() {
        Some("signup-agree-required")
    } else if !valid_login_name(&login_name) {
        Some("login-name-invalid")
    } else if display_name.is_empty() || display_name.chars().count() > 255 {
        Some("display-name-required")
    } else if login_name_conflicts_with_community(&mut tx, &login_name).await? {
        Some("login-name-conflict-error")
    } else if find_user_by_login_name(&mut tx, &login_name)
        .await?
        .is_some()
    {
        Some("login-name-taken")
    } else {
        None
    };
    if let Some(problem) = problem {
        drop(tx);
        // Shown on the page itself rather than after a redirect, so what was
        // typed is still there to correct.
        let error = safe_get_message(&bundle, problem);
        let html = welcome_page(
            &state,
            messages,
            ftl_lang,
            &pending,
            &login_name,
            &display_name,
            Some(error),
        )?;
        return Ok(html.into_response());
    }

    let identity = &pending.identity;
    let user = create_user(
        &mut tx,
        UserDraft::without_password(login_name, display_name),
        &state.config,
    )
    .await?;
    if let Some(email) = identity.email.clone() {
        update_user_email_verified_at(&mut tx, user.id, email, Utc::now()).await?;
    }
    if let Some(lang) = detect_preferred_language(&accept_language) {
        update_user_preferred_language(&mut tx, user.id, Some(lang)).await?;
    }
    if link_identity(&mut tx, user.id, identity).await?.is_err() {
        // Linked to someone else between the sign-in and now: another tab
        // made an account with it first. Make nothing.
        drop(tx);
        let _ = session.remove::<PendingIdentity>(PENDING_KEY).await;
        messages
            .clone()
            .error(say(&bundle, "identity-linked-elsewhere", identity.provider));
        return Ok(Redirect::to("/login").into_response());
    }
    tx.commit().await?;

    let _ = session.remove::<PendingIdentity>(PENDING_KEY).await;
    auth_session
        .login(&user)
        .await
        .map_err(|_| AppError::Unauthorized)?;
    welcome(&messages, &bundle, &user);
    Ok(Redirect::to(pending.next.as_deref().unwrap_or("/")).into_response())
}

/// Forgets the identity waiting in the session.
pub async fn cancel_pending_identity(session: Session) -> impl IntoResponse {
    let _ = session.remove::<PendingIdentity>(PENDING_KEY).await;
    Redirect::to("/login")
}

pub async fn do_unlink_identity(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Response, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let bundle = bundle_for(&accept_language, Some(user));
    let Some(provider) = Provider::parse(&provider) else {
        return Err(AppError::NotFound("Provider".to_string()));
    };

    let mut tx = state.db_pool.begin().await?;
    match unlink_identity(&mut tx, user, provider).await? {
        Ok(()) => {
            tx.commit().await?;
            messages
                .clone()
                .success(say(&bundle, "identity-unlinked", provider));
        }
        Err(UnlinkError::NotLinked) => {}
        Err(UnlinkError::LastSignIn) => {
            messages
                .clone()
                .error(safe_get_message(&bundle, "identity-unlink-last"));
        }
    }
    Ok(Redirect::to("/account").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_on_this_site_are_gone_on_to() {
        assert_eq!(local_next(Some("/draw")).as_deref(), Some("/draw"));
        assert_eq!(
            local_next(Some("/@someone/abc?x=1")).as_deref(),
            Some("/@someone/abc?x=1")
        );
        for away in [
            "https://evil.test/",
            "//evil.test",
            "/\\evil.test",
            "evil",
            "",
            "/a\nb",
        ] {
            assert_eq!(local_next(Some(away)), None, "{away:?}");
        }
        assert_eq!(local_next(None), None);
    }

    #[test]
    fn only_this_sites_pages_may_post_an_identity() {
        let with = |origin: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
            headers
        };
        let site = "https://oeee.cafe";
        assert!(from_this_site(&with("https://oeee.cafe"), site));
        assert!(from_this_site(
            &with("https://oeee.cafe"),
            "https://oeee.cafe/"
        ));
        assert!(!from_this_site(&with("https://evil.test"), site));
        assert!(!from_this_site(&with("http://oeee.cafe"), site));
        assert!(!from_this_site(&with("https://oeee.cafe.evil.test"), site));
        assert!(!from_this_site(&with("null"), site));
        assert!(!from_this_site(&HeaderMap::new(), site));
    }

    #[test]
    fn handles_are_what_the_users_table_allows() {
        assert!(valid_login_name("oeee_cafe-1"));
        assert!(valid_login_name(&"a".repeat(50)));
        assert!(!valid_login_name(&"a".repeat(51)));
        assert!(!valid_login_name(""));
        assert!(!valid_login_name("오이"));
        assert!(!valid_login_name("a b"));
    }

    #[test]
    fn a_suggested_handle_keeps_what_a_handle_can_hold() {
        assert_eq!(suggested_login_name(Some("Cucumber Fan!")), "CucumberFan");
        assert_eq!(suggested_login_name(Some("오이")), "");
        assert_eq!(suggested_login_name(None), "");
        assert!(valid_login_name(&suggested_login_name(Some(
            &"x".repeat(80)
        ))));
    }
}
