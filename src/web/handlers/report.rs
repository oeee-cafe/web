use crate::app_error::AppError;
use crate::models::post::find_post_by_id;
use crate::models::user::{find_user_by_id, find_user_by_login_name, AuthSession};
use crate::web::handlers::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::Form;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use lettre::transport::smtp::authentication::Credentials as SmtpCredentials;
use lettre::{Message, SmtpTransport, Transport};
use minijinja::context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ReportPostRequest {
    pub description: String,
}

#[derive(Serialize)]
pub struct ReportPostResponse {
    pub message: String,
}

/// API endpoint: POST /api/v1/posts/:post_id/report
pub async fn report_post_api(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Json(request): Json<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Ensure user is authenticated
    let user = auth_session
        .user
        .ok_or(AppError::Unauthorized)?;

    // Validate description
    if request.description.trim().is_empty() {
        return Err(AppError::InvalidFormData(
            "Report description is required".to_string(),
        ));
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Fetch post details
    let post = find_post_by_id(&mut tx, post_id).await?;

    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }

    let post = post.unwrap();

    // Check if post is deleted
    if post.get("deleted_at").and_then(|v| v.as_ref()).is_some() {
        return Err(AppError::InvalidFormData("Cannot report deleted post".to_string()));
    }

    // Get post author ID and check if user is trying to report their own post
    let post_author_id = post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::DatabaseError("Invalid post author ID".to_string()))?;

    if post_author_id == user.id {
        return Err(AppError::InvalidFormData(
            "You cannot report your own post".to_string(),
        ));
    }

    // Fetch post author details
    let post_author = find_user_by_id(&mut tx, post_author_id)
        .await?
        .ok_or(AppError::DatabaseError("Post author not found".to_string()))?;

    tx.commit().await?;

    // Prepare email content
    let post_url = format!("https://{}/@{}/{}", state.config.domain, post_author.login_name, post_id);
    let reporter_profile_url = format!("https://{}/@{}", state.config.domain, user.login_name);
    let post_author_profile_url = format!(
        "https://{}/@{}",
        state.config.domain, post_author.login_name
    );

    let post_title = post
        .get("title")
        .and_then(|v| v.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("[No title]");

    let post_content = post
        .get("content")
        .and_then(|v| v.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("[No content]");

    let email_body = format!(
        "A post has been reported on oeee.cafe

Reporter Information:
- Username: {} (@{})
- User ID: {}
- Profile: {}

Reported Post:
- Post ID: {}
- Title: {}
- Content: {}
- URL: {}

Post Author:
- Username: {} (@{})
- User ID: {}
- Profile: {}

Report Reason:
{}

---
This is an automated report notification from oeee.cafe",
        user.display_name,
        user.login_name,
        user.id,
        reporter_profile_url,
        post_id,
        post_title,
        post_content,
        post_url,
        post_author.display_name,
        post_author.login_name,
        post_author.id,
        post_author_profile_url,
        request.description
    );

    // Get bundle for localized from address
    let bundle = get_bundle(&accept_language, None);
    let from_address = safe_get_message(&bundle, "email-from-address");

    // Send email to abuse@oeee.cafe
    let email_message = Message::builder()
        .from(
            from_address
                .parse()
                .map_err(|e: lettre::address::AddressError| {
                    AppError::DatabaseError(format!("Invalid from address: {}", e))
                })?,
        )
        .to("abuse@oeee.cafe"
            .parse()
            .map_err(|e: lettre::address::AddressError| {
                AppError::DatabaseError(format!("Invalid to address: {}", e))
            })?)
        .subject(format!(
            "Post Report: {} (Post ID: {})",
            post_title, post_id
        ))
        .body(email_body)
        .map_err(|e| AppError::DatabaseError(format!("Failed to build email: {}", e)))?;

    let mailer = SmtpTransport::relay(&state.config.smtp_host)
        .map_err(|e| AppError::DatabaseError(format!("Failed to create SMTP transport: {}", e)))?
        .credentials(SmtpCredentials::new(
            state.config.smtp_user.clone(),
            state.config.smtp_password.clone(),
        ))
        .build();

    mailer
        .send(&email_message)
        .map_err(|e| AppError::DatabaseError(format!("Failed to send email: {}", e)))?;

    Ok((
        StatusCode::CREATED,
        Json(ReportPostResponse {
            message: "Post reported successfully".to_string(),
        }),
    ))
}

/// API endpoint: POST /api/v1/profiles/:login_name/report
pub async fn report_profile_api(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Json(request): Json<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Ensure user is authenticated
    let user = auth_session
        .user
        .ok_or(AppError::Unauthorized)?;

    // Validate description
    if request.description.trim().is_empty() {
        return Err(AppError::InvalidFormData(
            "Report description is required".to_string(),
        ));
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Fetch profile details
    let reported_user = find_user_by_login_name(&mut tx, &login_name).await?;

    if reported_user.is_none() {
        return Err(AppError::NotFound("Profile".to_string()));
    }

    let reported_user = reported_user.unwrap();

    // Check if user is trying to report their own profile
    if reported_user.id == user.id {
        return Err(AppError::InvalidFormData(
            "You cannot report your own profile".to_string(),
        ));
    }

    tx.commit().await?;

    // Prepare email content
    let profile_url = format!("https://{}/@{}", state.config.domain, reported_user.login_name);
    let reporter_profile_url = format!("https://{}/@{}", state.config.domain, user.login_name);

    let email_body = format!(
        "A user profile has been reported on oeee.cafe

Reporter Information:
- Username: {} (@{})
- User ID: {}
- Profile: {}

Reported Profile:
- Username: {} (@{})
- User ID: {}
- Profile: {}
- Display Name: {}

Report Reason:
{}

---
This is an automated report notification from oeee.cafe",
        user.display_name,
        user.login_name,
        user.id,
        reporter_profile_url,
        reported_user.display_name,
        reported_user.login_name,
        reported_user.id,
        profile_url,
        reported_user.display_name,
        request.description
    );

    // Get bundle for localized from address
    let bundle = get_bundle(&accept_language, None);
    let from_address = safe_get_message(&bundle, "email-from-address");

    // Send email to abuse@oeee.cafe
    let email_message = Message::builder()
        .from(
            from_address
                .parse()
                .map_err(|e: lettre::address::AddressError| {
                    AppError::DatabaseError(format!("Invalid from address: {}", e))
                })?,
        )
        .to("abuse@oeee.cafe"
            .parse()
            .map_err(|e: lettre::address::AddressError| {
                AppError::DatabaseError(format!("Invalid to address: {}", e))
            })?)
        .subject(format!(
            "Profile Report: {} (@{})",
            reported_user.display_name, reported_user.login_name
        ))
        .body(email_body)
        .map_err(|e| AppError::DatabaseError(format!("Failed to build email: {}", e)))?;

    let mailer = SmtpTransport::relay(&state.config.smtp_host)
        .map_err(|e| AppError::DatabaseError(format!("Failed to create SMTP transport: {}", e)))?
        .credentials(SmtpCredentials::new(
            state.config.smtp_user.clone(),
            state.config.smtp_password.clone(),
        ))
        .build();

    mailer
        .send(&email_message)
        .map_err(|e| AppError::DatabaseError(format!("Failed to send email: {}", e)))?;

    Ok((
        StatusCode::CREATED,
        Json(ReportPostResponse {
            message: "Profile reported successfully".to_string(),
        }),
    ))
}

/// The same two reports, submitted from the site instead of from the phone.
///
/// Both delegate to the API handlers above rather than restating any of it, so
/// the mail that reaches abuse@ is identical whichever client sent it. What
/// differs is the reply: the browser gets a rendered sentence to swap into the
/// modal it was submitted from, where the JSON body and its status would be
/// useless.
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
    let template = state.env.get_template("report_result.jinja")?;
    let rendered = template.render(context! { message_key => key, ftl_lang })?;
    Ok(Html(rendered).into_response())
}

/// POST /posts/:post_id/report
pub async fn hx_report_post(
    auth_session: AuthSession,
    accept_language: ExtractAcceptLanguage,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(post_id): Path<Uuid>,
    Form(form): Form<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = report_post_api(
        auth_session,
        accept_language,
        State(state.clone()),
        Path(post_id),
        Json(form),
    )
    .await;

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
    accept_language: ExtractAcceptLanguage,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Form(form): Form<ReportPostRequest>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = report_profile_api(
        auth_session,
        accept_language,
        State(state.clone()),
        Path(login_name),
        Json(form),
    )
    .await;

    let key = match outcome {
        Ok(_) => "profile-report-success",
        Err(e) => {
            tracing::warn!("profile report from the web form failed: {}", e);
            "profile-report-error"
        }
    };
    render_report_result(&state, &ftl_lang, key).await
}
