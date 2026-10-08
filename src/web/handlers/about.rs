use crate::app_error::AppError;
use crate::build_info::git_commit;
use crate::models::supporter::list_credits;
use crate::models::user::{find_users_with_public_posts_and_banner, AuthSession};
use crate::web::context::CommonContext;
use crate::web::state::AppState;
use axum::{extract::State, response::Html};
use minijinja::value::Serde;

use crate::web::i18n::ExtractFtlLang;
use minijinja::context;

pub async fn about(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let users_with_public_posts_and_banner = find_users_with_public_posts_and_banner(&mut tx)
        .await
        .unwrap_or_default();
    let supporters = list_credits(&mut tx).await?;

    let rendered: String = state
        .render_page(
            "about.jinja",
            common_ctx,
            context! {
                users_with_public_posts_and_banner => Serde(users_with_public_posts_and_banner),
                supporters => Serde(supporters),
                git_commit => git_commit(),
            },
        )
        .await?;

    Ok(Html(rendered))
}

/// The design system's reference page: every token and component in
/// `static/ds.css`, drawn by the real stylesheet. Unlinked; see design.jinja.
pub async fn design(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;
    let rendered = state
        .render_page(
            "design.jinja",
            common_ctx,
            context! {
                messages => Vec::<String>::new(),
            },
        )
        .await?;
    Ok(Html(rendered))
}
