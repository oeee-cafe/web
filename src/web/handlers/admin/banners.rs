//! The queue of profile banners, and flagging them.

use crate::app_error::AppError;
use crate::models::admin::{find_all_banners, find_banner_by_id, set_banner_explicit};
use crate::web::context::CommonContext;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use axum::Form;
use minijinja::context;
use minijinja::value::Serde;
use serde::Deserialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const BANNERS_PER_PAGE: i64 = 60;

#[derive(Debug, Deserialize)]
pub struct AdminBannersQuery {
    /// Row offset for the infinite-scroll sentinel. The first page omits it.
    pub offset: Option<i64>,
    /// `?explicit=on` narrows to already-flagged banners, for reviewing past
    /// decisions.
    pub explicit: Option<String>,
}

/// Loads one batch of banners plus the sentinel URL for the next. Shared by the
/// full page and the fragment so both stay in step.
async fn load_banner_batch(
    tx: &mut Transaction<'_, Postgres>,
    query: &AdminBannersQuery,
) -> Result<(Vec<crate::models::admin::AdminBanner>, bool, String), AppError> {
    let offset = query.offset.unwrap_or(0).max(0);
    let only_explicit = query.explicit.is_some();

    let banners = find_all_banners(tx, only_explicit, BANNERS_PER_PAGE, offset).await?;
    let has_more = banners.len() as i64 == BANNERS_PER_PAGE;

    let mut next_url = format!(
        "/admin/banners-fragment?offset={}",
        offset + BANNERS_PER_PAGE
    );
    if only_explicit {
        next_url.push_str("&explicit=on");
    }

    Ok((banners, has_more, next_url))
}

/// GET /admin/banners — banner review queue. Flagged banners are withheld from
/// the public /about page.
pub async fn admin_banners(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminBannersQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let (banners, has_more, next_url) = load_banner_batch(&mut tx, &query).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let rendered = state
        .render_page(
            "admin/banners.jinja",
            common_ctx,
            context! {
                banners => Serde(banners),
                only_explicit => query.explicit.is_some(),
                has_more => has_more,
                next_url => next_url,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}

/// GET /admin/banners-fragment — one batch of banner cards plus the next
/// sentinel, for htmx to swap in.
pub async fn admin_banners_fragment(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<AdminBannersQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let (banners, has_more, next_url) = load_banner_batch(&mut tx, &query).await?;
    tx.commit().await?;

    let rendered = state
        .render(
            "admin/banners_fragment.jinja",
            context! {
                banners => Serde(banners),
                has_more => has_more,
                next_url => next_url,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[derive(Debug, Deserialize)]
pub struct FlagBannerForm {
    /// Desired end state, not a toggle, so a double-submit is idempotent.
    pub is_explicit: bool,
}

/// POST /admin/banners/:banner_id/explicit — flag or unflag a banner. Returns
/// the replacement card for htmx to swap in place.
pub async fn admin_flag_banner(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
    Form(form): Form<FlagBannerForm>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    set_banner_explicit(&mut tx, banner_id, form.is_explicit, admin.0.id).await?;

    // Re-read so the card reflects what actually landed, including flagged_at.
    let banner = find_banner_by_id(&mut tx, banner_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Banner".to_string()))?;
    tx.commit().await?;

    let rendered = state
        .render(
            "admin/banner_card.jinja",
            context! {
                banner => Serde(banner),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}
