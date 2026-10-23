//! The lists of users and of communities.

use super::range::DateRange;
use crate::app_error::AppError;
use crate::models::admin::{
    find_all_communities_with_activity, find_all_users, find_user_achievements, find_user_detail,
    find_user_devices, find_user_identities, find_user_pickles_given,
    find_user_supporter_purchases, AdminSort, AdminUserFilter, AdminUserStat,
};
use crate::models::identity::Provider;
use crate::web::context::CommonContext;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
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

/// GET /admin/users/:login_name — one account, deleted or not: its row,
/// its sign-ins, the Supporter Packs it bought, the pickles it gave, its
/// achievements and the devices it is pushed to. Every username on the
/// admin pages leads here; its posts are a link away.
pub async fn admin_user_detail(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let user = find_user_detail(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;
    let identities = find_user_identities(&mut tx, user.id).await?;
    let purchases = find_user_supporter_purchases(&mut tx, user.id).await?;
    let pickles_given = find_user_pickles_given(&mut tx, user.id).await?;
    let achievements = find_user_achievements(&mut tx, user.id).await?;
    let devices = find_user_devices(&mut tx, user.id).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let rendered = state
        .render_page(
            "admin/user_detail.jinja",
            common_ctx,
            context! {
                user => Serde(user),
                identities => Serde(identities),
                purchases => Serde(purchases),
                pickles_given => Serde(pickles_given),
                achievements => Serde(achievements),
                devices => Serde(devices),
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
    let communities = find_all_communities_with_activity(&mut tx, query.sort, None).await?;
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
