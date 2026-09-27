use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::state::AppState;
use axum::{extract::State, response::Html};

use minijinja::context;

use crate::web::i18n::ExtractFtlLang;

pub async fn policy(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let rendered = state
        .render_page("policy.jinja", common_ctx, context! {})
        .await?;

    Ok(Html(rendered))
}
