//! The Supporter Pack's own page: what a pack is, this year's, and the
//! button that asks the app to sell it.
//!
//! Only an app can sell one -- StoreKit in the iOS app, Steam's overlay in
//! the Steam build -- so the page is written for both and shows whichever
//! button the page is being read in front of (`theme_head.jinja` marks the
//! root, `static/style.css` hides the rest). A browser gets the same page
//! without a button, which is the honest answer: there is nothing the site
//! itself can take money with.

use axum::{extract::State, response::Html};
use minijinja::context;

use crate::app_error::AppError;
use crate::models::supporter::{current_year, mark_for, standings};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::state::AppState;

use super::ExtractFtlLang;

pub async fn supporter_page(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let year = current_year();
    // This year's pack in each store, where there is one. A store with
    // nothing for this year yet sells nothing, and says so rather than
    // offering a button that would open an empty sheet.
    let apple_pack = state.config.app_store.as_ref().and_then(|config| {
        config
            .supporter_products
            .iter()
            .find(|pack| pack.year == year)
            .map(|pack| pack.product_id.clone())
    });
    let steam_pack = state.config.steam.as_ref().and_then(|config| {
        config
            .supporter_apps
            .iter()
            .find(|pack| pack.year == year && pack.app_id != config.app_id)
            .map(|pack| pack.app_id)
    });

    let (supporter_standings, worn_mark) = match auth_session.user.as_ref() {
        Some(user) => (
            standings(&mut tx, user.id).await?,
            mark_for(&mut tx, user.id).await?,
        ),
        None => (Vec::new(), None),
    };
    // The stores this year's pack has already been bought in. Buying it
    // again there is the store's own "you already own this" sheet, so the
    // button goes -- but the other store's stays, because a second pack for
    // the same year is supporting twice and that is allowed.
    let bought_in: Vec<String> = supporter_standings
        .iter()
        .filter(|standing| standing.year == year)
        .map(|standing| standing.store.clone())
        .collect();
    let supports_this_year = !bought_in.is_empty();

    let template: minijinja::Template<'_, '_> = state.env.get_template("supporter.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        this_year => year,
        apple_pack,
        steam_pack,
        supporter_standings,
        worn_mark,
        supports_this_year,
        bought_in,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;
    Ok(Html(rendered))
}
