//! What the stores sold, handed over by the page: `POST
//! /store/:store/purchases`, with the proof of one purchase in `proof`. And,
//! for the one store that needs the site's help to make its proof, `POST
//! /store/microsoft/tickets`.
//!
//! An app does only what its store can -- sell, restore, hand over proof --
//! and gives the proof to the page, which posts it here from inside the web
//! view (app_store.jinja), so the session it is signed in as and the page's
//! own origin come with it. What the proof is depends on the store, and none
//! of it is taken on its word: each is taken back to the store that issued
//! it, and only what that store says is recorded.
//!
//! - `apple`: a StoreKit transaction id, which the App Store Server API is
//!   asked about (`crate::app_store`).
//! - `steam`: a Web API ticket, which Steam is asked who it names and what
//!   that account owns (`crate::steam`).
//! - `microsoft`: a Microsoft Store ID key, which the collections API is
//!   asked what its account owns (`crate::microsoft_store`). The app makes
//!   the key from a ticket the site hands out here first.
//! - `google`: a Google Play purchase token, which the Google Play
//!   Developer API is asked about, and which the site then acknowledges
//!   (`crate::google_play`).
//!
//! Which products count is the catalogue's to say (`models::store_product`):
//! every product it has for that store, on sale or not, since a pack taken
//! off sale still counts for whoever bought it. The year a purchase is
//! credited with is the catalogue's too.
//!
//! Every store answers the same way, which is what the page reads: 204 when
//! the purchase is recorded, and anything else leaves it to be offered again
//! -- 401 with nobody to record it for, 429 when the site has asked too
//! often, 502 when the store could not be reached, 400 for proof the store
//! does not recognise, and 404 from a store this deployment is not set up to
//! ask, or has never heard of.
//!
//! And from Apple itself, `POST /store/apple/notifications`: App Store
//! Server Notifications, which say a purchase has been refunded or the like
//! (`app_store::read_notification`).
//!
//! These are purchases, not sign-ins: nothing here signs anyone in or links
//! an identity (see `models::supporter`), so a page that is mid-drawing is
//! left as it was.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use serde::Deserialize;
use serde_json::json;

use crate::app_error::AppError;
use crate::app_store;
use crate::google_play;
use crate::microsoft_store::{self, KeyRejected};
use crate::models::identity::{find_user_by_identity, refresh_standing, Provider};
use crate::models::store_product;
use crate::models::supporter::{record_purchase, Store};
use crate::models::user::AuthSession;
use crate::steam::{self, TicketRejected};
use crate::web::handlers::identity::from_this_site;
use crate::web::state::AppState;

#[derive(Deserialize)]
pub struct PurchaseForm {
    /// What the store gave the app for the purchase, as the page was handed
    /// it: a transaction id for the App Store, a hex-encoded Web API ticket
    /// for Steam, a Store ID key for the Microsoft Store, a purchase token
    /// for Google Play.
    proof: String,
}

pub async fn do_store_purchase(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(store): Path<String>,
    headers: HeaderMap,
    Form(form): Form<PurchaseForm>,
) -> Result<Response, AppError> {
    // An Origin from somewhere else is turned away before anything is asked
    // of a store, whichever store it names.
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    match Store::parse(&store) {
        Some(Store::Apple) => apple_purchase(auth_session, &state, &form.proof).await,
        Some(Store::Steam) => steam_purchase(auth_session, &state, &form.proof).await,
        Some(Store::Microsoft) => microsoft_purchase(auth_session, &state, &form.proof).await,
        Some(Store::Google) => google_play_purchase(auth_session, &state, &form.proof).await,
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

/// A ticket for the app to make a Microsoft Store ID key with: `{ticket,
/// user}`, which the app passes to `GetCustomerCollectionsIdAsync` as its
/// service ticket and publisher user id. Only the Microsoft Store asks for
/// one; any other store is a 404, as is a deployment with no
/// `[microsoft_store]`.
///
/// Signed in only, and from this site only: the ticket is a token of the
/// site's own, good for minutes, and `user` is whoever the key will be
/// asked about for. It lets the app ask the Store to name the account
/// signed into it, and nothing more.
pub async fn do_store_ticket(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(store): Path<String>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }
    let (Some(Store::Microsoft), Some(config)) =
        (Store::parse(&store), state.config.microsoft_store.as_ref())
    else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(user) = auth_session.user.as_ref() else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    if !microsoft_store::may_ask(user.id) {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    match microsoft_store::ticket(config).await {
        Ok(ticket) => Ok(Json(json!({
            "ticket": ticket,
            "user": microsoft_store::user_reference(user.id),
        }))
        .into_response()),
        Err(error) => {
            tracing::warn!("no Microsoft Store ticket could be made: {error:#}");
            Ok(StatusCode::BAD_GATEWAY.into_response())
        }
    }
}

/// Asks the App Store what a transaction was, and records it for whoever is
/// signed in. The app hands one over after a purchase goes through, after a
/// Restore, and for any transaction StoreKit has not seen finished.
///
/// The id is worth nothing on its own: Apple is asked what the transaction
/// was, and the pack goes to whoever is signed in here -- no Apple identity
/// needed, so buying inside the app works for an account that signs in with
/// a password.
///
/// Restoring hands the same transaction over again, which updates the one
/// row that purchase has: it can give a pack back, or hand it to the account
/// restoring it, but it cannot make a second one (see `models::supporter`).
async fn apple_purchase(
    auth_session: AuthSession,
    state: &AppState,
    transaction_id: &str,
) -> Result<Response, AppError> {
    let Some(config) = state.config.app_store.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(user) = auth_session.user.as_ref() else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    // Every post here is a request to Apple, against a rate limit the whole
    // site shares (app_store::may_ask).
    if !app_store::may_ask(user.id) {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let packs = store_product::packs_in(&state.db_pool, Store::Apple).await?;
    let purchase = match app_store::look_up(config, &packs, transaction_id).await {
        Ok(Some(purchase)) => purchase,
        // Not a transaction of ours, or not one Apple knows.
        Ok(None) => return Ok(StatusCode::BAD_REQUEST.into_response()),
        Err(error) => {
            tracing::warn!("an App Store purchase could not be checked: {error:#}");
            return Ok(StatusCode::BAD_GATEWAY.into_response());
        }
    };

    let mut tx = state.db_pool.begin().await?;
    record_purchase(
        &mut tx,
        user.id,
        Store::Apple,
        &purchase.transaction,
        &purchase.pack,
        purchase.owned,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Asks Steam which Supporter Packs the ticket's Steam account owns, and
/// records them for whoever is signed in. The app hands a ticket over when
/// Steam says a DLC has been installed -- a pack, bought in the overlay or
/// the store while the app was open -- so the mark follows at once rather
/// than at the next daily recheck.
///
/// The packs are the signed-in account's, so buying one inside the Steam app
/// works without linking Steam at all. With nobody signed in there is still
/// the account the Steam identity is linked to, if it is linked to one --
/// Steam is the store whose purchases a sign-in's identity owns
/// ([`Store::identity`]) -- and otherwise nothing to record against.
///
/// Unlike `/auth/steam` it signs nobody in and links nothing. Standing is
/// only ever what Steam says, so there is nothing here worth forging.
async fn steam_purchase(
    auth_session: AuthSession,
    state: &AppState,
    ticket: &str,
) -> Result<Response, AppError> {
    let Some(config) = state.config.steam.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let packs = store_product::packs_in(&state.db_pool, Store::Steam).await?;
    let identity = match steam::verify_ticket(config, &packs, ticket).await {
        Ok(Ok(identity)) => identity,
        Ok(Err(TicketRejected::Invalid)) => return Ok(StatusCode::BAD_REQUEST.into_response()),
        Ok(Err(TicketRejected::Banned)) => return Ok(StatusCode::FORBIDDEN.into_response()),
        Err(error) => {
            tracing::warn!("Steam standing could not be refreshed: {error:#}");
            return Ok(StatusCode::BAD_GATEWAY.into_response());
        }
    };
    let mut tx = state.db_pool.begin().await?;
    let holder = match auth_session.user.as_ref() {
        Some(user) => Some(user.id),
        None => find_user_by_identity(&mut tx, Provider::Steam, &identity.subject)
            .await?
            .map(|user| user.id),
    };
    let Some(holder) = holder else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    refresh_standing(&mut tx, holder, &identity).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Asks the Microsoft Store what the account behind a Store ID key bought,
/// and records every pack of it for whoever is signed in. The app hands a
/// key over after a purchase goes through, and whenever it finds one the
/// site has not taken.
///
/// The key is asked about with the id the ticket was handed out with, which
/// is the signed-in account's own. Like an App Store transaction, the
/// purchases go to whoever is signed in and no Microsoft account is linked:
/// the same order handed over by another account moves the one row it has
/// rather than making another (`microsoft_store`).
async fn microsoft_purchase(
    auth_session: AuthSession,
    state: &AppState,
    key: &str,
) -> Result<Response, AppError> {
    let Some(config) = state.config.microsoft_store.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(user) = auth_session.user.as_ref() else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    if !microsoft_store::may_ask(user.id) {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let packs = store_product::packs_in(&state.db_pool, Store::Microsoft).await?;
    if packs.is_empty() {
        // Nothing here was ever sold there, so nothing the Store could say
        // would be a pack.
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let reference = microsoft_store::user_reference(user.id);
    let purchases = match microsoft_store::look_up(config, &packs, key, &reference).await {
        Ok(Ok(purchases)) => purchases,
        Ok(Err(KeyRejected::Invalid)) => return Ok(StatusCode::BAD_REQUEST.into_response()),
        Err(error) => {
            tracing::warn!("a Microsoft Store purchase could not be checked: {error:#}");
            return Ok(StatusCode::BAD_GATEWAY.into_response());
        }
    };
    // An account that bought none of them: nothing of ours to record.
    if purchases.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let mut tx = state.db_pool.begin().await?;
    for purchase in &purchases {
        record_purchase(
            &mut tx,
            user.id,
            Store::Microsoft,
            &purchase.order,
            &purchase.pack,
            purchase.owned,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Asks Google Play what a purchase token was, records it for whoever is
/// signed in, and acknowledges it. The app hands a token over after a sale
/// goes through, after a Restore, and for any purchase Play lists that has
/// not been acknowledged yet.
///
/// Like an App Store transaction, the token is worth nothing on its own and
/// the pack goes to whoever is signed in here; restoring it on another
/// account moves the one row it has rather than making another.
///
/// Acknowledging comes after the purchase is recorded, so a purchase Play
/// keeps is one the site has. When it fails the answer is a 502, which
/// leaves the token to be handed over again -- recording it a second time
/// changes nothing, and acknowledging it is tried again -- before Play's
/// three days are up.
async fn google_play_purchase(
    auth_session: AuthSession,
    state: &AppState,
    token: &str,
) -> Result<Response, AppError> {
    let Some(config) = state.config.google_play.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(user) = auth_session.user.as_ref() else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    if !google_play::may_ask(user.id) {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let packs = store_product::packs_in(&state.db_pool, Store::Google).await?;
    let purchase = match google_play::look_up(config, &packs, token).await {
        Ok(Some(purchase)) => purchase,
        // Not a purchase of ours, or not one Google knows.
        Ok(None) => return Ok(StatusCode::BAD_REQUEST.into_response()),
        Err(error) => {
            tracing::warn!("a Google Play purchase could not be checked: {error:#}");
            return Ok(StatusCode::BAD_GATEWAY.into_response());
        }
    };

    let mut tx = state.db_pool.begin().await?;
    record_purchase(
        &mut tx,
        user.id,
        Store::Google,
        &purchase.token,
        &purchase.pack,
        purchase.owned,
    )
    .await?;
    tx.commit().await?;

    // Only a purchase that went through is Play's to refund by itself; one
    // still pending cannot be acknowledged yet.
    if purchase.owned && !purchase.acknowledged {
        if let Err(error) = google_play::acknowledge(config, &purchase).await {
            tracing::warn!("a Google Play purchase could not be acknowledged: {error:#}");
            return Ok(StatusCode::BAD_GATEWAY.into_response());
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct AppStoreNotification {
    #[serde(rename = "signedPayload")]
    signed_payload: String,
}

/// POST /store/apple/notifications -- App Store Server Notifications,
/// version 2, which App Store Connect is given as both the production and
/// the sandbox URL.
///
/// Anyone can post here, so nothing is done for a payload Apple did not
/// sign, and nothing a signed one says is recorded as it stands: it names a
/// transaction, which is looked up again, and what Apple says of it now is
/// recorded against the purchase that transaction already is. A transaction
/// no account has handed over is no purchase here yet, and is left for the
/// app to hand over.
///
/// Apple sends a notification again, for up to three days, until it is
/// answered with a 200. So everything that was read is a 200, whether or
/// not there was anything to do, and only Apple being out of reach is not.
pub async fn do_app_store_notification(
    State(state): State<AppState>,
    Json(body): Json<AppStoreNotification>,
) -> Result<Response, AppError> {
    let Some(config) = state.config.app_store.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let notice = match app_store::read_notification(
        &body.signed_payload,
        config,
        rustls_pki_types::UnixTime::now(),
    ) {
        Ok(notice) => notice,
        Err(error) => {
            tracing::info!("turned away an App Store notification: {error:#}");
            return Ok(StatusCode::BAD_REQUEST.into_response());
        }
    };
    let (kind, transaction, product) = match notice {
        app_store::Notice::Test => {
            tracing::info!("the App Store's test notification arrived");
            return Ok(StatusCode::OK.into_response());
        }
        app_store::Notice::Nothing { kind } => {
            tracing::debug!(kind, "an App Store notification with nothing to do");
            return Ok(StatusCode::OK.into_response());
        }
        app_store::Notice::Recheck {
            kind,
            transaction,
            product,
        } => (kind, transaction, product),
    };
    match app_store::heed(&state.db_pool, config, &transaction, &product).await {
        Ok(app_store::Heeded::Recorded { owned }) => {
            tracing::info!(kind, owned, "an App Store notification was checked");
        }
        Ok(app_store::Heeded::NotOurs) => {
            tracing::debug!(kind, "an App Store notification about nothing of ours");
        }
        Err(error) => {
            // Apple will send it again.
            tracing::warn!(
                kind,
                "an App Store notification could not be checked: {error:#}"
            );
            return Ok(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
    }
    Ok(StatusCode::OK.into_response())
}
