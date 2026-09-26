//! The Supporter Pack's own page: what a pack is, this year's, and the
//! buttons that ask the app to sell it.
//!
//! Only an app can sell one -- StoreKit in the iOS and macOS apps, Steam's
//! overlay in the Steam build, the Microsoft Store in that build of the
//! Windows app, Play Billing in the Android app -- and only through the
//! store it was built for. So the
//! buttons are chosen here, from the store the request's user agent names
//! ([`Store::from_user_agent`]): that store's products on sale for this
//! year, from the catalogue (`models::store_product`), and nothing of any
//! other store's. A browser names no store and gets the same page without a
//! button, which is the honest answer: there is nothing the site itself can
//! take money with.

use axum::http::header::USER_AGENT;
use axum::http::HeaderMap;
use axum::{extract::State, response::Html};
use minijinja::context;
use serde::Serialize;

use crate::app_error::AppError;
use crate::models::store_product;
use crate::models::supporter::{current_year, mark_for, standings, Store};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::state::AppState;

use super::ExtractFtlLang;

/// A button on the page: the product it asks the app to sell, and its own
/// words where staff gave it some.
#[derive(Serialize)]
struct Offer {
    product: String,
    label: Option<String>,
}

pub async fn supporter_page(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
    headers: HeaderMap,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let year = current_year();
    let store = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .and_then(Store::from_user_agent);

    let (supporter_standings, worn_mark) = match auth_session.user.as_ref() {
        Some(user) => (
            standings(&mut tx, user.id).await?,
            mark_for(&mut tx, user.id).await?,
        ),
        None => (Vec::new(), None),
    };
    // The stores this year's pack has already been bought in. Buying it
    // again there is the store's own "you already own this" sheet, so that
    // store offers nothing -- but another store would, because a second pack
    // for the same year is supporting twice and that is allowed.
    let bought_in: Vec<String> = supporter_standings
        .iter()
        .filter(|standing| standing.year == year)
        .map(|standing| standing.store.clone())
        .collect();
    let supports_this_year = !bought_in.is_empty();
    let bought_here = store.is_some_and(|store| bought_in.iter().any(|in_| in_ == store.as_str()));

    // This year's products in the reader's store, on sale. A store with
    // nothing for this year yet sells nothing, and says so rather than
    // offering a button that would open an empty sheet.
    let offers: Vec<Offer> = match store {
        Some(store) if !bought_here => store_product::list_on_sale(&mut tx, store, year)
            .await?
            .into_iter()
            .map(|product| Offer {
                product: product.product,
                label: product.label,
            })
            .collect(),
        _ => Vec::new(),
    };
    // "No pack this year" is said where it is true of what the reader could
    // buy: their store's, in an app, or every store's, in a browser.
    let nothing_this_year = match store {
        Some(_) => offers.is_empty() && !bought_here,
        None => {
            let mut none = true;
            for store in Store::ALL {
                if !store_product::list_on_sale(&mut tx, store, year)
                    .await?
                    .is_empty()
                {
                    none = false;
                    break;
                }
            }
            none
        }
    };

    let rendered = state
        .render(
            "supporter.jinja",
            context! {
                current_user => auth_session.user,
                this_year => year,
                store,
                offers,
                // Restoring is the App Store's word for it. Google Play has the same
                // thing -- what the device's Google account owns, handed over again
                // -- and Steam and the Microsoft Store say it every time they are
                // asked.
                restorable => matches!(store, Some(Store::Apple | Store::Google)),
                nothing_this_year,
                supporter_standings,
                worn_mark,
                supports_this_year,
                draft_post_count => common_ctx.draft_post_count,
                unread_notification_count => common_ctx.unread_notification_count,
                ftl_lang,
            },
        )
        .await?;
    Ok(Html(rendered))
}
