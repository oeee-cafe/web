//! Signing in with another service's account: Steam, Apple and Google now,
//! Microsoft after them.
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
//! Apple's does not -- Apple posts its answer from appleid.apple.com, which
//! is exactly the cross-site post the cookie is not sent with -- so it lands
//! on [`apple_callback`], which hands it straight back to this site from a
//! page of its own ([`do_apple_sign_in`]).
//!
//! Google comes back by a GET, which a Lax cookie is sent with, so
//! [`google_callback`] is the whole of it. Google will not sign in inside a
//! web view at all, so the apps go round it: the iOS, macOS and Windows apps
//! in a browser of the system's ([`handoff_start`]), and the Android app
//! with Credential Manager, whose ID token the page posts to
//! [`do_google_sign_in`] as the Steam app's page posts its ticket.

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
use crate::apple;
use crate::google;
use crate::models::identity::{
    find_user_by_identity, find_user_by_verified_email, link_identity, touch_identity,
    unlink_identity, LinkError, Provider, UnlinkError, VerifiedIdentity,
};
use crate::models::store_product;
use crate::models::supporter::Store;
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

/// Where to send someone back to when a sign-in did not happen.
fn back_for(auth_session: &AuthSession) -> &'static str {
    if auth_session.user.is_some() {
        "/account"
    } else {
        "/login"
    }
}

/// Whether a POST came from one of this site's own pages, by its `Origin`.
/// A request without one is turned away: every browser these forms are
/// posted from -- the Steam app's webview, and whatever a new account is
/// made in right after -- sends it.
pub(crate) fn from_this_site(headers: &HeaderMap, base_url: &str) -> bool {
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
            touch_identity(&mut tx, linked.id, &identity).await?;
            tx.commit().await?;
            auth_session
                .login(&linked)
                .await
                .map_err(|_| AppError::Unauthorized)?;
            welcome(messages, bundle, &linked);
            Ok(Redirect::to(next.as_deref().unwrap_or("/")).into_response())
        }
        (Some(linked), Some(current)) if linked.id == current.id => {
            touch_identity(&mut tx, linked.id, &identity).await?;
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
    /// A handoff this sign-in is being made on behalf of an app for
    /// (`crate::handoff`). Absent for an ordinary browser sign-in.
    handoff: Option<String>,
}

/// `/auth/steam/app` is the Steam sign-in button's link, which only the
/// Steam build of the app gets anything from: there the page takes the press
/// itself, asks the app for a Web API ticket and posts it to `/auth/steam`
/// (app_sign_in.jinja), so the link is never followed. A browser, or any
/// other build, follows it and lands here instead, to be told so.
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
    let back = back_for(&auth_session);

    let Some(config) = state.config.steam.as_ref() else {
        messages
            .clone()
            .error(safe_get_message(&bundle, "steam-sign-in-unavailable"));
        return Ok(Redirect::to(back).into_response());
    };

    // Signing in says what the account owns as well, against every Steam
    // product the catalogue has.
    let packs = store_product::packs_in(&state.db_pool, Store::Steam).await?;
    let identity = match steam::verify_ticket(config, &packs, &form.ticket).await {
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

const APPLE_REQUEST_KEY: &str = "identity.apple";

/// A sign-in with Apple under way: what Apple's answer has to carry back.
#[derive(Serialize, Deserialize)]
struct AppleRequest {
    state: String,
    nonce: String,
    next: Option<String>,
    started_at: DateTime<Utc>,
    /// The app's handoff this sign-in was started for (`crate::handoff`),
    /// kept with the sign-in itself rather than beside it in the session: a
    /// handoff someone started and walked away from must not take over the
    /// next sign-in this browser makes -- linking Apple on /account, say --
    /// which never asked to be handed anywhere. Absent for every sign-in
    /// begun without `?handoff=`, and in a request stored before there was
    /// one.
    #[serde(default)]
    handoff: Option<String>,
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn apple_redirect_uri(base_url: &str) -> String {
    format!("{}/auth/apple/callback", base_url.trim_end_matches('/'))
}

/// Sends the browser to Apple to sign in, remembering what its answer has to
/// carry back.
pub async fn apple_sign_in(
    auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Query(query): Query<NextQuery>,
) -> Result<Response, AppError> {
    let Some(config) = state.config.apple.as_ref() else {
        let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
        messages
            .clone()
            .error(safe_get_message(&bundle, "apple-sign-in-unavailable"));
        return Ok(Redirect::to(back_for(&auth_session)).into_response());
    };

    let request = AppleRequest {
        state: random_token(),
        nonce: random_token(),
        next: local_next(query.next.as_deref()),
        started_at: Utc::now(),
        handoff: pending_handoff(&state, query.handoff.as_deref()).await,
    };
    let url = apple::authorize_url(
        config,
        &apple_redirect_uri(&state.config.base_url),
        &request.state,
        &request.nonce,
    );
    session
        .insert(APPLE_REQUEST_KEY, request)
        .await
        .map_err(|e| AppError::InvalidFormData(e.to_string()))?;
    Ok(Redirect::to(&url).into_response())
}

#[derive(Deserialize)]
pub struct AppleStartForm {
    next: Option<String>,
}

/// A sign-in for the iOS and macOS apps to make natively: the state and
/// nonce they hand Apple, kept in the web view's session as `/auth/apple`
/// keeps them for a browser. The app asks from inside the page, so the
/// answer is this session's; another site's page gets neither the session
/// nor, without CORS, the answer.
pub async fn apple_start(
    session: Session,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AppleStartForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    if state.config.apple.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let request = AppleRequest {
        state: random_token(),
        nonce: random_token(),
        next: local_next(form.next.as_deref()),
        started_at: Utc::now(),
        handoff: None,
    };
    let answer = serde_json::json!({ "state": request.state, "nonce": request.nonce });
    session
        .insert(APPLE_REQUEST_KEY, request)
        .await
        .map_err(|e| AppError::InvalidFormData(e.to_string()))?;
    Ok(axum::Json(answer).into_response())
}

/// What Apple posts back: an ID token and the state it was sent with, or an
/// error such as `user_cancelled_authorize`. `code` is also posted, and not
/// needed: the ID token already says who signed in.
#[derive(Deserialize, Serialize)]
pub struct AppleAnswer {
    state: Option<String>,
    id_token: Option<String>,
    /// JSON with the person's name, the first time only.
    user: Option<String>,
    error: Option<String>,
    /// "json" from an app's page, which goes where this says rather than
    /// being sent ([`where_it_went`]). Absent from Apple's own post, which
    /// is a browser and is redirected.
    #[serde(default)]
    format: Option<String>,
}

/// Where Apple posts its answer. The post comes from appleid.apple.com, so it
/// arrives without the session cookie; this page only posts the same fields
/// on to `/auth/apple` from this site, where the cookie comes along. It reads
/// and writes nothing in the session -- touching it here would start a new,
/// empty one over the reader's.
pub async fn apple_callback(
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Form(answer): Form<AppleAnswer>,
) -> Result<Html<String>, AppError> {
    let rendered = state
        .render(
            "identity_apple_return.jinja",
            context! {
                answer,
                ftl_lang,
            },
        )
        .await?;
    Ok(Html(rendered))
}

pub async fn do_apple_sign_in(
    auth_session: AuthSession,
    session: Session,
    accept_language: ExtractAcceptLanguage,
    messages: Messages,
    state: State<AppState>,
    headers: HeaderMap,
    Form(answer): Form<AppleAnswer>,
) -> Result<Response, AppError> {
    let asked = asked_where(answer.format.as_deref());
    let went = apple_sign_in_going(
        auth_session,
        session,
        accept_language,
        messages,
        state,
        headers,
        answer,
    )
    .await?;
    Ok(if asked { where_it_went(went) } else { went })
}

#[allow(clippy::too_many_arguments)]
async fn apple_sign_in_going(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    headers: HeaderMap,
    answer: AppleAnswer,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    let back = back_for(&auth_session);

    // Each sign-in's state and nonce answer once.
    let request = session
        .remove::<AppleRequest>(APPLE_REQUEST_KEY)
        .await
        .ok()
        .flatten()
        .filter(|request| Utc::now() - request.started_at <= Duration::minutes(PENDING_FOR));
    let Some(config) = state.config.apple.as_ref() else {
        messages
            .clone()
            .error(safe_get_message(&bundle, "apple-sign-in-unavailable"));
        return Ok(Redirect::to(back).into_response());
    };
    if answer.error.as_deref() == Some("user_cancelled_authorize") {
        return Ok(Redirect::to(back).into_response());
    }
    let invalid = || -> Result<Response, AppError> {
        messages
            .clone()
            .error(say(&bundle, "identity-sign-in-invalid", Provider::Apple));
        Ok(Redirect::to(back).into_response())
    };
    let (Some(request), Some(id_token)) = (request, answer.id_token.as_deref()) else {
        return invalid();
    };
    if answer.state.as_deref() != Some(request.state.as_str()) {
        return invalid();
    }

    let identity = match apple::verify_id_token(
        config,
        id_token,
        &request.nonce,
        answer.user.as_deref(),
    )
    .await
    {
        Ok(Some(identity)) => identity,
        Ok(None) => return invalid(),
        Err(error) => {
            tracing::error!("Apple sign-in could not be checked: {error:#}");
            messages
                .clone()
                .error(say(&bundle, "identity-sign-in-failed", Provider::Apple));
            return Ok(Redirect::to(back).into_response());
        }
    };

    if let Some(done) = handed_off(&state, request.handoff.as_deref(), &identity).await {
        return Ok(done);
    }
    sign_in_with(
        &mut auth_session,
        &session,
        &messages,
        &bundle,
        &state,
        identity,
        request.next,
    )
    .await
}

const GOOGLE_REQUEST_KEY: &str = "identity.google";

/// A sign-in with Google under way: what Google's answer has to carry back.
#[derive(Serialize, Deserialize)]
struct GoogleRequest {
    state: String,
    nonce: String,
    next: Option<String>,
    started_at: DateTime<Utc>,
    /// The app's handoff this sign-in was started for; see [`AppleRequest`].
    #[serde(default)]
    handoff: Option<String>,
}

fn google_redirect_uri(base_url: &str) -> String {
    format!("{}/auth/google/callback", base_url.trim_end_matches('/'))
}

/// Sends the browser to Google to sign in, remembering what its answer has to
/// carry back.
pub async fn google_sign_in(
    auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Query(query): Query<NextQuery>,
) -> Result<Response, AppError> {
    let Some(config) = state.config.google.as_ref() else {
        let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
        messages
            .clone()
            .error(safe_get_message(&bundle, "google-sign-in-unavailable"));
        return Ok(Redirect::to(back_for(&auth_session)).into_response());
    };

    let request = GoogleRequest {
        state: random_token(),
        nonce: random_token(),
        next: local_next(query.next.as_deref()),
        started_at: Utc::now(),
        handoff: pending_handoff(&state, query.handoff.as_deref()).await,
    };
    let url = google::authorize_url(
        config,
        &google_redirect_uri(&state.config.base_url),
        &request.state,
        &request.nonce,
    );
    session
        .insert(GOOGLE_REQUEST_KEY, request)
        .await
        .map_err(|e| AppError::InvalidFormData(e.to_string()))?;
    Ok(Redirect::to(&url).into_response())
}

/// What Google sends the browser back with: a code and the state it was sent
/// with, or an error such as `access_denied`.
#[derive(Deserialize)]
pub struct GoogleAnswer {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

/// Where Google sends the browser back. A top-level GET, which the session
/// cookie is sent with, so the answer is read here rather than bounced off a
/// page of this site the way Apple's post is.
pub async fn google_callback(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Query(answer): Query<GoogleAnswer>,
) -> Result<Response, AppError> {
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    let back = back_for(&auth_session);

    // Each sign-in's state and nonce answer once: from this browser's session,
    // or -- for a browser an app sent straight to Google -- from the handoff
    // it was sent for.
    let mut request = take_google_request(&session)
        .await
        .filter(|request| answer.state.as_deref() == Some(request.state.as_str()));
    if request.is_none() {
        if let Some(oauth_state) = answer.state.as_deref() {
            request = sent_to_google(&state, oauth_state).await;
        }
    }
    let Some(config) = state.config.google.as_ref() else {
        messages
            .clone()
            .error(safe_get_message(&bundle, "google-sign-in-unavailable"));
        return Ok(Redirect::to(back).into_response());
    };
    // Put away without signing in: the page stays as it was.
    if answer.error.is_some() {
        return Ok(Redirect::to(back).into_response());
    }
    let invalid = || -> Result<Response, AppError> {
        messages
            .clone()
            .error(say(&bundle, "identity-sign-in-invalid", Provider::Google));
        Ok(Redirect::to(back).into_response())
    };
    let (Some(request), Some(code)) = (request, answer.code.as_deref()) else {
        return invalid();
    };
    if answer.state.as_deref() != Some(request.state.as_str()) {
        return invalid();
    }

    let id_token =
        match google::exchange_code(config, &google_redirect_uri(&state.config.base_url), code)
            .await
        {
            Ok(Some(id_token)) => id_token,
            Ok(None) => return invalid(),
            Err(error) => {
                // Not a refusal -- that is Ok(None), and said where it happens --
                // but Google not answering at all.
                tracing::error!("Google could not be reached to trade the code: {error:#}");
                messages
                    .clone()
                    .error(say(&bundle, "identity-sign-in-failed", Provider::Google));
                return Ok(Redirect::to(back).into_response());
            }
        };

    finish_google_sign_in(
        &mut auth_session,
        &session,
        &messages,
        &bundle,
        &state,
        config,
        &id_token,
        &request.nonce,
        request.next,
        request.handoff.as_deref(),
        back,
    )
    .await
}

#[derive(Deserialize)]
pub struct GoogleStartForm {
    next: Option<String>,
}

/// A sign-in for the Android app to make itself, with Credential Manager: the
/// nonce it hands Google, kept in the web view's session as `/auth/google`
/// keeps it for a browser. Google refuses its own sign-in pages inside an
/// embedded web view; the other apps sign in in a browser of the system's
/// instead ([`handoff_start`]).
///
/// The app asks from inside the page, so the answer is this session's;
/// another site's page gets neither the session nor, without CORS, the
/// answer.
pub async fn google_start(
    session: Session,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<GoogleStartForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    if state.config.google.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let request = GoogleRequest {
        state: random_token(),
        nonce: random_token(),
        next: local_next(form.next.as_deref()),
        started_at: Utc::now(),
        handoff: None,
    };
    // Android's Credential Manager is given the site's own OAuth client
    // (GoogleSignIn.kt), so its token names `[google].client_id` as audience,
    // the only one the site takes.
    let answer = serde_json::json!({ "state": request.state, "nonce": request.nonce });
    session
        .insert(GOOGLE_REQUEST_KEY, request)
        .await
        .map_err(|e| AppError::InvalidFormData(e.to_string()))?;
    Ok(axum::Json(answer).into_response())
}

/// What a phone app's page posts: the ID token the app ended up with and the
/// state the sign-in was started with, or an error when Google would not say
/// who this is.
#[derive(Deserialize)]
pub struct GoogleNativeAnswer {
    state: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
    /// "json" from an app's page; see [`where_it_went`].
    #[serde(default)]
    format: Option<String>,
}

/// An ID token a phone app signed in for, posted from the page.
pub async fn do_google_sign_in(
    auth_session: AuthSession,
    session: Session,
    accept_language: ExtractAcceptLanguage,
    messages: Messages,
    state: State<AppState>,
    headers: HeaderMap,
    Form(answer): Form<GoogleNativeAnswer>,
) -> Result<Response, AppError> {
    let asked = asked_where(answer.format.as_deref());
    let went = google_sign_in_going(
        auth_session,
        session,
        accept_language,
        messages,
        state,
        headers,
        answer,
    )
    .await?;
    Ok(if asked { where_it_went(went) } else { went })
}

#[allow(clippy::too_many_arguments)]
async fn google_sign_in_going(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    headers: HeaderMap,
    answer: GoogleNativeAnswer,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    let back = back_for(&auth_session);

    let request = take_google_request(&session).await;
    let Some(config) = state.config.google.as_ref() else {
        messages
            .clone()
            .error(safe_get_message(&bundle, "google-sign-in-unavailable"));
        return Ok(Redirect::to(back).into_response());
    };
    let invalid = || -> Result<Response, AppError> {
        messages
            .clone()
            .error(say(&bundle, "identity-sign-in-invalid", Provider::Google));
        Ok(Redirect::to(back).into_response())
    };
    let Some(request) = request else {
        return invalid();
    };
    if answer.state.as_deref() != Some(request.state.as_str()) {
        return invalid();
    }
    if answer.error.is_some() {
        return invalid();
    }
    let Some(id_token) = answer.id_token.as_deref() else {
        return invalid();
    };

    finish_google_sign_in(
        &mut auth_session,
        &session,
        &messages,
        &bundle,
        &state,
        config,
        id_token,
        &request.nonce,
        request.next,
        request.handoff.as_deref(),
        back,
    )
    .await
}

/// The state and nonce of a sign-in an app sent the browser straight to Google
/// for (`handoff::AtProvider`), carrying its handoff as `/auth/google?handoff=`
/// would have -- so everything after is the same as for a browser that came
/// through this site first.
async fn sent_to_google(state: &AppState, oauth_state: &str) -> Option<GoogleRequest> {
    let request = match crate::handoff::back_from_provider(&state.redis_pool, oauth_state).await {
        Ok(request) => request?,
        Err(error) => {
            tracing::warn!("A handoff sent to Google could not be looked up: {error:#}");
            return None;
        }
    };
    let handoff = pending_handoff(state, Some(&request.id)).await;
    Some(GoogleRequest {
        state: oauth_state.to_string(),
        nonce: request.nonce,
        next: None,
        started_at: Utc::now(),
        handoff,
    })
}

/// The sign-in's state and nonce, taken from the session so they answer once,
/// and only while the sign-in is still recent.
async fn take_google_request(session: &Session) -> Option<GoogleRequest> {
    session
        .remove::<GoogleRequest>(GOOGLE_REQUEST_KEY)
        .await
        .ok()
        .flatten()
        .filter(|request| Utc::now() - request.started_at <= Duration::minutes(PENDING_FOR))
}

/// Checking an ID token and going on with whoever it names: the same for a
/// token traded for a code and one Credential Manager handed the app.
#[allow(clippy::too_many_arguments)]
async fn finish_google_sign_in(
    auth_session: &mut AuthSession,
    session: &Session,
    messages: &Messages,
    bundle: &Bundle<'_>,
    state: &AppState,
    config: &crate::config::GoogleConfig,
    id_token: &str,
    nonce: &str,
    next: Option<String>,
    handoff: Option<&str>,
    back: &str,
) -> Result<Response, AppError> {
    let identity = match google::verify_id_token(config, id_token, nonce).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            messages
                .clone()
                .error(say(bundle, "identity-sign-in-invalid", Provider::Google));
            return Ok(Redirect::to(back).into_response());
        }
        Err(error) => {
            tracing::error!("Google sign-in could not be checked: {error:#}");
            messages
                .clone()
                .error(say(bundle, "identity-sign-in-failed", Provider::Google));
            return Ok(Redirect::to(back).into_response());
        }
    };

    if let Some(done) = handed_off(state, handoff, &identity).await {
        return Ok(done);
    }
    sign_in_with(
        auth_session,
        session,
        messages,
        bundle,
        state,
        identity,
        next,
    )
    .await
}

/// A handoff's id, short enough to follow one through the log and hashed so
/// the log never holds the id itself -- which is as good as the sign-in it
/// is waiting for (`crate::handoff`).
fn handoff_mark(id: &str) -> String {
    sha256::digest(id).chars().take(8).collect()
}

/// The handoff a browser sign-in is being started on behalf of, to keep with
/// that sign-in's request. Only an id that names a handoff still waiting is
/// kept: a made-up one is ignored rather than carried to the provider and
/// back for nothing.
async fn pending_handoff(state: &AppState, handoff: Option<&str>) -> Option<String> {
    let id = handoff?;
    match crate::handoff::is_pending(&state.redis_pool, id).await {
        Ok(true) => Some(id.to_string()),
        Ok(false) => None,
        Err(error) => {
            tracing::warn!("A handoff could not be looked up: {error:#}");
            None
        }
    }
}

/// Hands what the provider just said to the app waiting for it, if one is,
/// and answers where to leave the browser when it did.
///
/// Called as soon as an identity is verified and *before* anything is done
/// with it: a browser signing in on an app's behalf signs nobody in here,
/// makes no account and links nothing. It carries a message and stops. What
/// to do about the identity is the app session's to decide, in
/// [`handoff_claim`], where the person deciding is the one already signed
/// in there.
async fn handed_off(
    state: &AppState,
    handoff: Option<&str>,
    identity: &VerifiedIdentity,
) -> Option<Response> {
    let id = handoff?;
    let mark = handoff_mark(id);
    match crate::handoff::verified(&state.redis_pool, id, identity).await {
        // Claimed already, or waited too long: nothing is listening, so the
        // browser carries on as an ordinary sign-in.
        Ok(false) => {
            tracing::info!("handoff {mark}: nothing waiting for it; signing in here instead");
            None
        }
        Ok(true) => {
            tracing::info!(
                "handoff {mark}: {} said who this is; the app can take it",
                identity.provider.as_str()
            );
            Some(Redirect::to("/auth/handoff/done").into_response())
        }
        Err(error) => {
            tracing::error!("handoff {mark}: could not be handed to the app: {error:#}");
            None
        }
    }
}

/// Where a redirect was going, for an app's page to go there itself.
///
/// An app posts a sign-in from the page it is on and asks for this rather
/// than being redirected, so the page can `location.replace` instead of
/// following: the page signed in from is then not left in the history behind
/// the one it lands on, which is a Back that goes to the sign-in form of an
/// account already signed in.
///
/// The notice waits in the session either way -- nothing has rendered yet --
/// so it is shown by the page the app replaces with, once.
fn where_it_went(response: Response) -> Response {
    let next = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("/")
        .to_string();
    axum::Json(serde_json::json!({ "next": next })).into_response()
}

/// Whether an app asked to be told where to go instead of being sent.
fn asked_where(format: Option<&str>) -> bool {
    format == Some("json")
}

#[derive(Deserialize)]
pub struct HandoffStartForm {
    provider: String,
    next: Option<String>,
    /// "provider" sends the browser straight to the provider's own page
    /// rather than through this site's first (`handoff::AtProvider`). Only
    /// Google, and only asked for by the apps that will open Google's page:
    /// iOS and macOS, whose sheet names the first page's domain, and
    /// Windows, where it saves a stop here. Android opens only this site's
    /// URLs (app_sign_in.jinja).
    at: Option<String>,
}

/// Starts a handoff for the page in an app's web view: see `crate::handoff`.
///
/// The page keeps `secret` and opens `url` in a browser of the system's --
/// on Android that has to be a Custom Tab, or the app catches its own link.
pub async fn handoff_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<HandoffStartForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    // Only a provider this site actually offers, so an app is never sent to
    // a sign-in that would turn it away on arrival.
    let configured = match Provider::parse(&form.provider) {
        Some(Provider::Apple) => state.config.apple.is_some(),
        Some(Provider::Google) => state.config.google.is_some(),
        // Steam signs in from inside the app already; it needs no browser.
        Some(Provider::Steam) | None => false,
    };
    if !configured {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let next = local_next(form.next.as_deref());
    let started = crate::handoff::start(&state.redis_pool, next.clone()).await?;
    let url = match (form.at.as_deref(), state.config.google.as_ref()) {
        (Some("provider"), Some(config)) if form.provider == "google" => {
            let request = crate::handoff::AtProvider {
                id: started.id.clone(),
                nonce: random_token(),
            };
            let oauth_state = random_token();
            crate::handoff::send_to_provider(&state.redis_pool, &oauth_state, &request).await?;
            google::authorize_url(
                config,
                &google_redirect_uri(&state.config.base_url),
                &oauth_state,
                &request.nonce,
            )
        }
        _ => format!(
            "{}/auth/{}?handoff={}",
            state.config.base_url.trim_end_matches('/'),
            form.provider,
            started.id
        ),
    };
    Ok(axum::Json(serde_json::json!({
        "id": started.id,
        "secret": started.secret,
        "url": url,
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct HandoffClaimForm {
    id: String,
    secret: String,
    /// Sent on the second ask, once the person has seen what will be linked.
    confirm: Option<String>,
}

/// Claims a handoff: takes what the provider said in the browser and does
/// with it, here, what an ordinary sign-in would have done -- which is how
/// linking stays safe. The session that acts is this one, the app's, the one
/// holding the secret; a browser that merely knew the id could put an
/// identity into the handoff but can never make this session accept it
/// unseen.
///
/// Without `confirm`, a handoff that would be *linked* to the account
/// already signed in here is only described, not acted on, so the app can
/// show whose account it is about to attach and let it be refused. Signing
/// in as nobody in particular needs no such question.
pub async fn handoff_claim(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<HandoffClaimForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let say = |status: &str| axum::Json(serde_json::json!({ "status": status })).into_response();

    let mark = handoff_mark(&form.id);
    let looked = crate::handoff::peek(&state.redis_pool, &form.id, &form.secret).await?;
    let (identity, next) = match looked {
        crate::handoff::Claim::Waiting => return Ok(say("waiting")),
        crate::handoff::Claim::Unknown => {
            tracing::info!("handoff {mark}: claimed, but there is no such handoff to give");
            return Ok(say("unknown"));
        }
        crate::handoff::Claim::Ready { identity, next } => (*identity, next),
    };

    // Linking to the account already signed in here: say what it is and
    // wait to be told to go ahead. The handoff is left unspent, so refusing
    // costs nothing and asking again is free.
    if auth_session.user.is_some() && form.confirm.is_none() {
        let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
        let account = identity
            .name
            .clone()
            .or_else(|| identity.email.clone())
            .unwrap_or_default();
        let mut args = FluentArgs::new();
        args.set(
            "provider",
            FluentValue::from(identity.provider.display_name()),
        );
        args.set("account", FluentValue::from(account.clone()));
        // Worded here, where the reader's language is known: the page asking
        // is a script in an app's assets and has no messages of its own.
        let key = if account.is_empty() {
            "handoff-confirm-link-unnamed"
        } else {
            "handoff-confirm-link"
        };
        return Ok(axum::Json(serde_json::json!({
            "status": "confirm",
            "provider": identity.provider.display_name(),
            "account": account,
            "message": safe_format_message(&bundle, key, Some(&args)),
        }))
        .into_response());
    }

    // Acted on first, and spent only once it has taken.
    //
    // The other way round loses a sign-in outright: spending first meant
    // that anything going wrong afterwards -- and `sign_in_with` can fail --
    // left the handoff already gone, so every later ask got `unknown` and
    // the person was left on the page they started from with nothing said.
    // Acting twice, if two claims race, costs nothing: signing the same
    // session in as the same account, or linking an identity already linked
    // to it, both land where they already are.
    let bundle = bundle_for(&accept_language, auth_session.user.as_ref());
    let provider = identity.provider;
    let signed_in_before = auth_session.user.is_some();
    let response = match sign_in_with(
        &mut auth_session,
        &session,
        &messages,
        &bundle,
        &state,
        identity,
        next.clone(),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            // Left unspent on purpose: whatever went wrong, the sign-in
            // waiting in it has not been used, and asking again may work.
            tracing::error!(
                "handoff {mark}: {} sign-in failed: {error:?}",
                provider.as_str()
            );
            return Ok(say("failed"));
        }
    };
    let spent = crate::handoff::spend(&state.redis_pool, &form.id, &form.secret).await?;
    tracing::info!(
        "handoff {mark}: {} claimed; signed in here: {} -> {}; spent: {spent}",
        provider.as_str(),
        signed_in_before,
        auth_session.user.is_some(),
    );
    // `sign_in_with` answers with a redirect -- to `next`, to /account, or
    // to /auth/welcome when there is no account yet and one has to be named.
    // The page follows it itself, so it is told where rather than sent.
    let go = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/")
        .to_string();
    Ok(axum::Json(serde_json::json!({ "status": "ready", "next": go })).into_response())
}

/// What the browser is left on once it has signed in for an app. The app is
/// going on by itself; there is nothing more to do in this window.
pub async fn handoff_done(
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let rendered = state
        .render(
            "identity_handoff_done.jinja",
            context! {
                messages => messages.into_iter().collect::<Vec<_>>(),
                ftl_lang,
            },
        )
        .await?;
    Ok(Html(rendered))
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

async fn welcome_page(
    state: &AppState,
    messages: Messages,
    ftl_lang: String,
    pending: &PendingIdentity,
    login_name: &str,
    display_name: &str,
    error: Option<String>,
) -> Result<Html<String>, AppError> {
    let rendered = state
        .render(
            "identity_welcome.jinja",
            context! {
                messages => messages.into_iter().collect::<Vec<_>>(),
                ftl_lang,
                provider => pending.identity.provider.display_name(),
                provider_name => pending.identity.name,
                login_name,
                display_name,
                error,
                next => pending.next,
            },
        )
        .await?;
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
    )
    .await?
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
        )
        .await?;
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

    /// A sign-in's handoff travels in its own request, so one begun without
    /// `?handoff=` carries none, and a request stored by the release before
    /// this one -- still in someone's session across a deploy -- reads back
    /// as carrying none rather than failing to read at all.
    #[test]
    fn a_sign_in_is_handed_off_only_when_it_was_started_for_it() {
        let before = serde_json::json!({
            "state": "s",
            "nonce": "n",
            "next": "/account",
            "started_at": "2026-09-24T00:00:00Z",
        });
        let apple: AppleRequest = serde_json::from_value(before.clone()).expect("reads");
        assert_eq!(apple.handoff, None);
        let google: GoogleRequest = serde_json::from_value(before).expect("reads");
        assert_eq!(google.handoff, None);

        let started = AppleRequest {
            state: "s".into(),
            nonce: "n".into(),
            next: None,
            started_at: Utc::now(),
            handoff: Some("h".into()),
        };
        let kept: AppleRequest =
            serde_json::from_value(serde_json::to_value(&started).unwrap()).unwrap();
        assert_eq!(kept.handoff.as_deref(), Some("h"));
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
