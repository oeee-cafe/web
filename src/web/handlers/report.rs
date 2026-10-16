use crate::app_error::AppError;
use crate::models::post::find_post_by_id;
use crate::models::user::{find_user_by_id, find_user_by_login_name, AuthSession};
use crate::staff_webhook::{self, Event};
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::response::IntoResponse;
use axum::Form;
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ReportPostRequest {
    pub description: String,
}

/// Tells staff about a post, in their Discord channel (`crate::staff_webhook`).
async fn send_post_report(
    auth_session: AuthSession,
    state: &AppState,
    post_id: Uuid,
    request: ReportPostRequest,
) -> Result<(), AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    if request.description.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Report description is required".to_string(),
        ));
    }

    let mut tx = state.db_pool.begin().await?;
    let post = find_post_by_id(&mut tx, post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if post.get("deleted_at").and_then(|v| v.as_ref()).is_some() {
        return Err(AppError::BadRequest(
            "Cannot report deleted post".to_string(),
        ));
    }

    let post_author_id = post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::Anyhow(anyhow::anyhow!("Invalid post author ID")))?;

    if post_author_id == user.id {
        return Err(AppError::BadRequest(
            "You cannot report your own post".to_string(),
        ));
    }

    let post_author = find_user_by_id(&mut tx, post_author_id)
        .await?
        .ok_or_else(|| AppError::Anyhow(anyhow::anyhow!("Post author not found")))?;
    tx.commit().await?;

    deliver(Event::PostReported {
        reporter: user.login_name.to_string(),
        author: post_author.login_name.to_string(),
        post_id,
        title: post.get("title").cloned().flatten(),
        reason: request.description,
    })
}

/// Tells staff about a profile, in their Discord channel.
async fn send_profile_report(
    auth_session: AuthSession,
    state: &AppState,
    login_name: String,
    request: ReportPostRequest,
) -> Result<(), AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    if request.description.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Report description is required".to_string(),
        ));
    }

    let mut tx = state.db_pool.begin().await?;
    let reported_user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("Profile".to_string()))?;

    if reported_user.id == user.id {
        return Err(AppError::BadRequest(
            "You cannot report your own profile".to_string(),
        ));
    }
    tx.commit().await?;

    deliver(Event::ProfileReported {
        reporter: user.login_name.to_string(),
        reported: reported_user.login_name.to_string(),
        reason: request.description,
    })
}

/// Hands a report to the staff webhook. A deployment without one has
/// nowhere to send it, and the reporter is told it did not go through
/// rather than thanked for a report nobody will read.
fn deliver(event: Event) -> Result<(), AppError> {
    if !staff_webhook::is_on() {
        return Err(AppError::Anyhow(anyhow::anyhow!(
            "staff_webhook_url is not configured, so reports go nowhere"
        )));
    }
    staff_webhook::notify(event);
    Ok(())
}

/// The answer to either report form: a rendered sentence to swap into the
/// modal it was submitted from.
///
/// Failures come back as 200 with the message inside, deliberately. These
/// forms sit in a modal over a dimmed page, and `web::htmx::error_banner`
/// renders into the site header — which is behind that dim. An answer the
/// reporter cannot see is no better than the silence this replaces.
async fn render_report_result(
    state: &AppState,
    ftl_lang: &str,
    key: &str,
) -> Result<axum::response::Response, AppError> {
    let rendered = state
        .render(
            "report_result.jinja",
            context! { message_key => key, ftl_lang },
        )
        .await?;
    Ok(Html(rendered).into_response())
}

/// POST /posts/:post_id/report
pub async fn hx_report_post(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Form(form): Form<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = send_post_report(auth_session, &state, post_id, form).await;

    let key = match outcome {
        Ok(_) => "post-report-success",
        Err(e) => {
            tracing::warn!("post report from the web form failed: {}", e);
            "post-report-error"
        }
    };
    render_report_result(&state, &ftl_lang, key).await
}

/// POST /@:login_name/report
pub async fn hx_report_profile(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Form(form): Form<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = send_profile_report(auth_session, &state, login_name, form).await;

    let key = match outcome {
        Ok(_) => "profile-report-success",
        Err(e) => {
            tracing::warn!("profile report from the web form failed: {}", e);
            "profile-report-error"
        }
    };
    render_report_result(&state, &ftl_lang, key).await
}
