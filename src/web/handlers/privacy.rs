use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::state::AppState;
use axum::{extract::State, response::Html};

use minijinja::context;

use super::ExtractFtlLang;

pub async fn privacy(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let rendered = state
        .render(
            "privacy.jinja",
            context! {
                current_user => auth_session.user,
                draft_post_count => common_ctx.draft_post_count,
                unread_notification_count => common_ctx.unread_notification_count,
                ftl_lang,
            },
        )
        .await?;

    Ok(Html(rendered))
}
