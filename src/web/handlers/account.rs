use crate::app_error::{error_codes, AppError};
use crate::web::responses::ErrorResponse;
use crate::models::email_verification_challenge::{
    create_email_verification_challenge, find_email_verification_challenge_by_id,
};
use crate::models::user::{
    delete_user_with_activity, find_user_by_id, update_password, update_user_email_verified_at,
    update_user_preferred_language, update_user_show_sensitive_content, update_user_with_activity,
    AuthSession, DeleteConfirmation, Language,
};
use crate::models::identity::list_identities_for_user;
use crate::web::context::CommonContext;
use crate::web::handlers::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::state::AppState;
use axum::response::{IntoResponse, Redirect};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    Form, Json,
};
use axum_messages::Messages;
use chrono::{TimeDelta, Utc};
use fluent::FluentResource;
use intl_memoizer::concurrent::IntlLangMemoizer;
use lettre::transport::smtp::authentication::Credentials as SmtpCredentials;
use lettre::{Message, SmtpTransport, Transport};
use minijinja::context;
use rand::{thread_rng, Rng};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct EmailVerificationChallengeResponseForm {
    pub challenge_id: Uuid,
    pub token: String,
}

pub async fn account(
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    let identities = match auth_session.user.as_ref() {
        Some(user) => list_identities_for_user(&mut tx, user.id).await?,
        None => Vec::new(),
    };
    let has_password = auth_session.user.as_ref().is_some_and(|u| u.has_password());

    let languages = vec![
        ("ko", "한국어"),
        ("ja", "日本語"),
        ("en", "English"),
        ("zh", "中文"),
    ];
    let template: minijinja::Template<'_, '_> = state.env.get_template("account.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        languages,
        steam_linked => identities.iter().any(|i| i.provider == "steam"),
        apple_linked => identities.iter().any(|i| i.provider == "apple"),
        identities,
        has_password,
        steam_enabled => state.config.steam.is_some(),
        apple_enabled => state.config.apple.is_some(),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        messages => messages.into_iter().collect::<Vec<_>>(),
        ftl_lang
    })?;

    Ok(Html(rendered))
}

#[derive(Deserialize)]
pub struct LanguageEditForm {
    pub language: Option<String>,
}

pub async fn save_language(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Form(form): Form<LanguageEditForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let language = match form.language.as_deref() {
        Some("ko") => Some(Language::Ko),
        Some("ja") => Some(Language::Ja),
        Some("en") => Some(Language::En),
        Some("zh") => Some(Language::Zh),
        _ => None,
    };
    let _ = update_user_preferred_language(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        language,
    )
    .await;
    let _ = tx.commit().await;

    Ok(Redirect::to("/account").into_response())
}

#[derive(Deserialize)]
pub struct ShowSensitiveContentForm {
    pub show_sensitive_content: Option<String>,
}

pub async fn save_show_sensitive_content(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Form(form): Form<ShowSensitiveContentForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let show_sensitive_content = form.show_sensitive_content.as_deref() == Some("on");
    let _ = update_user_show_sensitive_content(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        show_sensitive_content,
    )
    .await;
    let _ = tx.commit().await;

    Ok(Redirect::to("/account").into_response())
}

#[derive(Deserialize)]
pub struct EditPasswordForm {
    /// Absent when the account has no password yet and this sets its first.
    #[serde(default)]
    current_password: String,
    new_password: String,
    new_password_confirm: String,
}

pub async fn edit_password(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Form(form): Form<EditPasswordForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = current_user.id;
    let user = find_user_by_id(&mut tx, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // An account made with an identity provider sets its first password
    // without one to give; the session is all it has to show.
    if user.has_password() && user.verify_password(&form.current_password).is_err() {
        messages.error(safe_get_message(
            &bundle,
            "account-change-password-error-incorrect-current",
        ));
        return Ok(Redirect::to("/account").into_response());
    }
    if form.new_password != form.new_password_confirm {
        messages.error(safe_get_message(
            &bundle,
            "account-change-password-error-new-mismatch",
        ));
        return Ok(Redirect::to("/account").into_response());
    }
    if form.new_password.len() < 8 {
        messages.error(safe_get_message(
            &bundle,
            "account-change-password-error-too-short",
        ));
        return Ok(Redirect::to("/account").into_response());
    }
    let _ = update_password(&mut tx, user_id, form.new_password).await;
    let _ = tx.commit().await;

    messages.success(safe_get_message(&bundle, "account-change-password-success"));
    Ok(Redirect::to("/account").into_response())
}

pub async fn verify_email_verification_code(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Form(form): Form<EmailVerificationChallengeResponseForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let challenge = find_email_verification_challenge_by_id(&mut tx, form.challenge_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Email verification challenge".to_string()))?;
    let now = Utc::now();

    let template: minijinja::Template<'_, '_> = state.env.get_template("email_verify.jinja")?;
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);

    if challenge.token != form.token {
        let ftl_lang = bundle
            .locales
            .first()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "en".to_string());
        let rendered = template.render(context! {
            challenge_id => challenge.id,
            email => challenge.email,
            message => safe_get_message(&bundle, "account-change-email-error-token-mismatch"),
            success => false,
            ftl_lang
        })?;

        return Ok(Html(rendered).into_response());
    }

    if challenge.expires_at < now {
        let ftl_lang = bundle
            .locales
            .first()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "en".to_string());
        let rendered = template.render(context! {
            challenge_id => challenge.id,
            email => challenge.email,
            message => safe_get_message(&bundle, "account-change-email-error-token-expired"),
            success => false,
            ftl_lang
        })?;

        return Ok(Html(rendered).into_response());
    }

    let _ = update_user_email_verified_at(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        challenge.clone().email,
        now,
    )
    .await;
    let _ = tx.commit().await;

    let ftl_lang = bundle
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string());
    let rendered = template.render(context! {
        challenge_id => challenge.id,
        email => challenge.email,
        message => safe_get_message(&bundle, "account-change-email-success"),
        success => true,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct RequestEmailVerificationCodeForm {
    email: String,
}

pub async fn request_email_verification_code(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Form(form): Form<RequestEmailVerificationCodeForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);
    let edit_email_template = state.env.get_template("email_edit.jinja")?;

    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    if current_user
        .email
        .as_ref()
        .is_some_and(|email| email == &form.email)
        && current_user.email_verified_at.is_some()
    {
        let ftl_lang = bundle
            .locales
            .first()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "en".to_string());
        return Ok(Html(edit_email_template.render(context! {
            current_user => auth_session.user,
            message => safe_get_message(&bundle, "account-change-email-error-already-verified"),
            ftl_lang,
        })?)
        .into_response());
    }

    // Use shared helper function to create challenge and send email
    let email_verification_challenge = create_and_send_verification_email(
        &state,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        &form.email,
        &bundle,
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("email_verify.jinja")?;
    let ftl_lang = bundle
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string());

    let rendered = template.render(context! {
        challenge_id => email_verification_challenge.id,
        email => form.email,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct EditUserForm {
    login_name: String,
    display_name: String,
}

pub async fn edit_account(
    messages: Messages,
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Form(form): Form<EditUserForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let _ = update_user_with_activity(
        &mut tx,
        user_id,
        form.login_name,
        form.display_name,
        &state.config,
        Some(&state),
    )
    .await;
    let _ = tx.commit().await;

    messages.success(safe_get_message(&bundle, "account-info-edit-success"));
    Ok(Redirect::to("/account").into_response())
}

#[derive(Deserialize)]
pub struct DeleteAccountRequest {
    /// For an account with a password.
    password: Option<String>,
    /// For one without: the account's own handle, typed out.
    login_name: Option<String>,
}

pub async fn delete_account(
    mut auth_session: AuthSession,
    State(state): State<AppState>,
    Json(payload): Json<DeleteAccountRequest>,
) -> Result<impl IntoResponse, AppError> {
    let user = match auth_session.user.as_ref() {
        Some(user) => user.clone(),
        None => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::UNAUTHORIZED,
                    "Not authenticated",
                )),
            )
                .into_response())
        }
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Attempt to delete the user
    match delete_user_with_activity(
        &mut tx,
        user.id,
        DeleteConfirmation::for_user(
            &user,
            payload.password.as_deref(),
            payload.login_name.as_deref(),
        ),
        &state.config,
        Some(&state),
    )
    .await
    {
        Ok(_) => {
            tx.commit().await?;

            // Log the user out
            auth_session.logout().await?;

            Ok(StatusCode::NO_CONTENT.into_response())
        }
        Err(e) => {
            tx.rollback().await?;

            Ok((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    error_codes::VALIDATION_ERROR,
                    e.to_string(),
                )),
            )
                .into_response())
        }
    }
}

#[derive(Deserialize)]
pub struct DeleteAccountForm {
    password: Option<String>,
    login_name: Option<String>,
}

pub async fn delete_account_htmx(
    mut auth_session: AuthSession,
    State(state): State<AppState>,
    Query(form): Query<DeleteAccountForm>,
) -> Result<impl IntoResponse, AppError> {
    let user = match auth_session.user.as_ref() {
        Some(user) => user.clone(),
        None => {
            return Ok((StatusCode::OK, [("HX-Redirect", "/login")]).into_response());
        }
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Attempt to delete the user
    match delete_user_with_activity(
        &mut tx,
        user.id,
        DeleteConfirmation::for_user(&user, form.password.as_deref(), form.login_name.as_deref()),
        &state.config,
        Some(&state),
    )
    .await
    {
        Ok(_) => {
            tx.commit().await?;

            // Log the user out
            auth_session.logout().await?;

            // Redirect to homepage with HX-Redirect header
            Ok((StatusCode::OK, [("HX-Redirect", "/")]).into_response())
        }
        Err(e) => {
            // Don't commit the transaction on error
            let _ = tx.rollback().await;

            // Return error message as HTML for HTMX to display
            // Basic HTML escaping for safety
            let error_msg = e
                .to_string()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            let error_html = format!(r#"<p class="error" style="color: red;">{}</p>"#, error_msg);
            Ok((StatusCode::OK, Html(error_html)).into_response())
        }
    }
}

// JSON API endpoints for mobile apps

use crate::models::email_verification_challenge::EmailVerificationChallenge;

// Helper function to create verification challenge and send email
async fn create_and_send_verification_email(
    state: &AppState,
    user_id: Uuid,
    email: &str,
    bundle: &fluent::bundle::FluentBundle<&FluentResource, IntlLangMemoizer>,
) -> Result<EmailVerificationChallenge, String> {
    let db = &state.db_pool;
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;

    // Generate 6-digit token
    let token = {
        let mut rng = thread_rng();
        (0..6)
            .map(|_| rng.gen_range(0..10).to_string())
            .collect::<Vec<String>>()
            .join("")
    };

    let expires_at =
        Utc::now() + TimeDelta::try_seconds(60 * 5).expect("5 minutes is a valid duration");

    let email_verification_challenge =
        create_email_verification_challenge(&mut tx, user_id, email, &token, expires_at)
            .await
            .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;

    // Send email
    let email_message = Message::builder()
        .from(
            safe_get_message(bundle, "email-from-address")
                .parse()
                .map_err(|e: lettre::address::AddressError| e.to_string())?,
        )
        .to(email
            .parse()
            .map_err(|e: lettre::address::AddressError| e.to_string())?)
        .subject(safe_get_message(bundle, "account-change-email-subject"))
        .body(token.clone())
        .map_err(|e| format!("Failed to build email message: {}", e))?;

    let mailer = SmtpTransport::relay(&state.config.smtp_host)
        .map_err(|e| format!("Failed to create SMTP transport: {}", e))?
        .credentials(SmtpCredentials::new(
            state.config.smtp_user.clone(),
            state.config.smtp_password.clone(),
        ))
        .build();

    mailer.send(&email_message).map_err(|e| e.to_string())?;

    Ok(email_verification_challenge)
}

#[derive(serde::Deserialize)]
pub struct RequestEmailVerificationJson {
    email: String,
}

#[derive(serde::Serialize)]
pub struct RequestEmailVerificationResponseJson {
    challenge_id: Uuid,
    email: String,
    expires_in_seconds: i64,
}

pub async fn request_email_verification_json(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Json(payload): Json<RequestEmailVerificationJson>,
) -> Result<impl IntoResponse, AppError> {
    let user = match auth_session.user.as_ref() {
        Some(user) => user.clone(),
        None => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::UNAUTHORIZED,
                    "Not authenticated",
                )),
            )
                .into_response())
        }
    };

    // Check if email is already verified
    if user.email.as_ref() == Some(&payload.email) && user.email_verified_at.is_some() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                error_codes::EMAIL_ALREADY_VERIFIED,
                "Email is already verified",
            )),
        )
            .into_response());
    }

    // Validate email format - basic check for @ symbol and parseable email
    if !payload.email.contains('@') || payload.email.parse::<lettre::Address>().is_err() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                error_codes::VALIDATION_ERROR,
                "Invalid email format",
            )),
        )
            .into_response());
    }

    let user_preferred_language = user.preferred_language;
    let bundle = get_bundle(&accept_language, user_preferred_language);

    // Create challenge and send email using shared helper
    match create_and_send_verification_email(&state, user.id, &payload.email, &bundle).await {
        Ok(email_verification_challenge) => Ok((
            StatusCode::OK,
            Json(RequestEmailVerificationResponseJson {
                challenge_id: email_verification_challenge.id,
                email: payload.email,
                expires_in_seconds: 300,
            }),
        )
            .into_response()),
        Err(e) => Ok((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                error_codes::INTERNAL_ERROR,
                format!("Failed to send email: {}", e),
            )),
        )
            .into_response()),
    }
}

#[derive(serde::Deserialize)]
pub struct VerifyEmailCodeJson {
    challenge_id: Uuid,
    token: String,
}

pub async fn verify_email_code_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Json(payload): Json<VerifyEmailCodeJson>,
) -> Result<impl IntoResponse, AppError> {
    let user = match auth_session.user.as_ref() {
        Some(user) => user.clone(),
        None => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::UNAUTHORIZED,
                    "Not authenticated",
                )),
            )
                .into_response())
        }
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let challenge = find_email_verification_challenge_by_id(&mut tx, payload.challenge_id).await?;

    if challenge.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new(
                error_codes::NOT_FOUND,
                "Verification challenge not found",
            )),
        )
            .into_response());
    }

    let challenge =
        challenge.ok_or_else(|| AppError::NotFound("Email verification challenge".to_string()))?;
    let now = Utc::now();

    // Check if token matches
    if challenge.token != payload.token {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                error_codes::INVALID_VERIFICATION_CODE,
                "Invalid verification code",
            )),
        )
            .into_response());
    }

    // Check if token is expired
    if challenge.expires_at < now {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                error_codes::VALIDATION_ERROR,
                "Verification code has expired",
            )),
        )
            .into_response());
    }

    // Update user email and verification timestamp
    update_user_email_verified_at(&mut tx, user.id, challenge.email, now).await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn get_account_json(auth_session: AuthSession) -> Result<impl IntoResponse, AppError> {
    let user = match auth_session.user.as_ref() {
        Some(user) => user.clone(),
        None => return Ok((StatusCode::UNAUTHORIZED).into_response()),
    };

    Ok((StatusCode::OK, Json(user)).into_response())
}
