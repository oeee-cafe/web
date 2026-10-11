//! The lists of users and of communities.

use super::range::DateRange;
use crate::app_error::AppError;
use crate::models::admin::{
    find_all_communities_with_activity, find_all_users, AdminSort, AdminUserFilter, AdminUserStat,
};
use crate::models::identity::Provider;
use crate::web::context::CommonContext;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Query, State};
use axum::response::Html;
use minijinja::context;
use minijinja::value::Serde;
use serde::{Deserialize, Serialize};

const USERS_PER_PAGE: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub page: Option<i64>,
    /// Missing or unrecognised sorts fall back to last-active.
    #[serde(default)]
    pub sort: AdminSort,
}

/// The user list's query: the list's own page and sort, and the overview
/// figure it was opened from, if it was.
#[derive(Debug, Deserialize)]
pub struct AdminUsersQuery {
    pub page: Option<i64>,
    #[serde(default)]
    pub sort: AdminSort,
    pub stat: Option<AdminUserStat>,
    pub provider: Option<Provider>,
    /// Without either, a figure is counted over all time.
    pub from: Option<String>,
    pub to: Option<String>,
}

/// The figure a filtered list was opened from, as its heading shows it.
#[derive(Debug, Serialize)]
struct UserFilterContext {
    description: String,
    /// `None` for a figure no range narrows, such as accounts linked now.
    range: Option<String>,
    matching: i64,
}

/// GET /admin/users — every account, deleted ones included. Sorted by last
/// activity by default.
///
/// `?stat=` narrows it to the accounts one figure on the overview counted,
/// over the overview's `from` and `to`; deleted accounts are left out then,
/// as they are from the figure.
pub async fn admin_users(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminUsersQuery>,
) -> Result<Html<String>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * USERS_PER_PAGE;

    let filter = match query.stat {
        None => None,
        Some(stat) if stat.needs_provider() && query.provider.is_none() => {
            return Err(AppError::BadRequest(
                "This figure is about a provider; name one with ?provider=".to_string(),
            ));
        }
        Some(stat) => {
            let range = if stat.is_timeless() {
                DateRange::default()
            } else {
                DateRange::parse(query.from.as_deref(), query.to.as_deref())?.unwrap_or_default()
            };
            Some((stat, range))
        }
    };
    // Kept on the sort links and the pager, so neither drops the figure.
    let filter_query = filter.map_or_else(String::new, |(stat, range)| {
        let mut params = format!("&stat={}", stat.as_str());
        if let Some(provider) = query.provider {
            params.push_str(&format!("&provider={}", provider.as_str()));
        }
        params.push('&');
        params.push_str(&range.query());
        params
    });

    let mut tx = state.db_pool.begin().await?;
    let (users, matching) = find_all_users(
        &mut tx,
        query.sort,
        filter.map(|(stat, range)| AdminUserFilter {
            stat,
            provider: query.provider,
            range: range.bounds(),
        }),
        USERS_PER_PAGE,
        offset,
    )
    .await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let has_next = users.len() as i64 == USERS_PER_PAGE;
    let rendered = state
        .render_page(
            "admin/users.jinja",
            common_ctx,
            context! {
                users => Serde(users),
                filter => filter.map(|(stat, range)| Serde(UserFilterContext {
                    description: stat.describe(query.provider),
                    range: (!stat.is_timeless()).then(|| range.describe()),
                    matching,
                })),
                filter_query => filter_query,
                page => page,
                sort => Serde(query.sort),
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
                communities => Serde(communities),
                sort => Serde(query.sort),
            },
        )
        .await?;

    Ok(Html(rendered))
}
