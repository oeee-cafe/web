//! The landing page: what has happened lately, and what is waiting on staff.

use super::range::{today, DateRange};
use crate::app_error::AppError;
use crate::models::admin::{
    find_all_collaborative_sessions, find_all_posts, find_all_users, find_overview,
    find_sign_in_methods, AdminPostFilter, AdminSessionStatus, AdminSort,
};
use crate::web::context::CommonContext;
use crate::web::handlers::collaborate::preview::preview_versions;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Query, State};
use axum::response::Html;
use minijinja::context;
use minijinja::value::Serde;
use serde::Deserialize;
use uuid::Uuid;

/// One row of the posts grid at its default six columns.
const RECENT_POSTS: i64 = 6;
const RECENT_SIGNUPS: i64 = 10;
const LIVE_SESSIONS: i64 = 6;
/// The range the overview opens on.
const DEFAULT_DAYS: i64 = 7;

/// `from` and `to` as `YYYY-MM-DD`; see `DateRange::parse`.
#[derive(Debug, Deserialize)]
pub struct AdminOverviewQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// GET /admin — the figures for a range of days, each linking to the accounts
/// it counted, then the newest of each list, each linking on to its full
/// section.
pub async fn admin_overview(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminOverviewQuery>,
) -> Result<Html<String>, AppError> {
    let today = today();
    let range = DateRange::parse(query.from.as_deref(), query.to.as_deref())?
        .unwrap_or_else(|| DateRange::last_days(today, DEFAULT_DAYS));

    let mut tx = state.db_pool.begin().await?;
    let overview = find_overview(&mut tx, range.bounds()).await?;
    let sign_in_methods = find_sign_in_methods(&mut tx, range.bounds()).await?;
    let posts = find_all_posts(&mut tx, AdminPostFilter::default(), RECENT_POSTS, 0).await?;
    let (signups, _) = find_all_users(&mut tx, AdminSort::Created, None, RECENT_SIGNUPS, 0).await?;
    let mut sessions = find_all_collaborative_sessions(
        &mut tx,
        AdminSort::Active,
        AdminSessionStatus::Live,
        LIVE_SESSIONS,
        0,
    )
    .await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    // Outside the transaction, as on the sessions list: a Redis round trip.
    let room_uuids: Vec<Uuid> = sessions.iter().map(|session| session.id).collect();
    for (session, version) in sessions
        .iter_mut()
        .zip(preview_versions(&state, &room_uuids).await)
    {
        session.preview_version = version;
    }

    let rendered = state
        .render_page(
            "admin/overview.jinja",
            common_ctx,
            context! {
                range => Serde(range.context()),
                presets => Serde(range.presets(today)),
                overview => Serde(overview),
                sign_in_methods => Serde(sign_in_methods),
                posts => Serde(posts),
                signups => Serde(signups),
                sessions => Serde(sessions),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered))
}
