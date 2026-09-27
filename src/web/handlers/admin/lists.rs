//! The lists of users and of communities.

use crate::app_error::AppError;
use crate::models::admin::{find_all_communities_with_activity, find_all_users, AdminSort};
use crate::web::context::CommonContext;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Query, State};
use axum::response::Html;
use minijinja::context;
use serde::Deserialize;

const USERS_PER_PAGE: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub page: Option<i64>,
    /// Missing or unrecognised sorts fall back to last-active.
    #[serde(default)]
    pub sort: AdminSort,
}

/// GET /admin/users — every account, deleted ones included. Sorted by last
/// activity by default.
pub async fn admin_users(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminListQuery>,
) -> Result<Html<String>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * USERS_PER_PAGE;

    let mut tx = state.db_pool.begin().await?;
    let users = find_all_users(&mut tx, query.sort, USERS_PER_PAGE, offset).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let has_next = users.len() as i64 == USERS_PER_PAGE;
    let rendered = state
        .render_page(
            "admin/users.jinja",
            common_ctx,
            context! {
                users => users,
                page => page,
                sort => query.sort,
                has_next => has_next,
            },
        )
        .await?;

    Ok(Html(rendered))
}

/// GET /admin/communities — every community, private and unlisted included.
/// Sorted by last activity by default.
pub async fn admin_communities(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminListQuery>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let communities = find_all_communities_with_activity(&mut tx, query.sort).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let rendered = state
        .render_page(
            "admin/communities.jinja",
            common_ctx,
            context! {
                communities => communities,
                sort => query.sort,
            },
        )
        .await?;

    Ok(Html(rendered))
}
