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
    /// for Steam, a Store ID key for the Microsoft Store.
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
