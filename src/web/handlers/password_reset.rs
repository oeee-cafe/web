use crate::app_error::AppError;
use crate::models::password_reset_challenge::{
    create_password_reset_challenge, delete_password_reset_challenges_for_user,
    find_password_reset_challenge_by_token, PasswordResetChallenge,
};
use crate::models::user::{find_user_by_email, update_password};
use crate::web::handlers::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::state::AppState;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::Form;
use axum_messages::Messages;
use chrono::{TimeDelta, Utc};
use fluent::FluentResource;
use intl_memoizer::concurrent::IntlLangMemoizer;
use lettre::transport::smtp::authentication::Credentials as SmtpCredentials;
use lettre::{Message, SmtpTransport, Transport};
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

// Web form handlers

pub async fn password_reset_request_page(
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let rendered = state
        .render(
            "password_reset_request.jinja",
            context! {
                messages => messages.into_iter().collect::<Vec<_>>(),
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[derive(Deserialize)]
pub struct PasswordResetRequestForm {
    pub email: String,
}

pub async fn password_reset_request(
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Form(form): Form<PasswordResetRequestForm>,
) -> Result<impl IntoResponse, AppError> {
    let bundle = get_bundle(&accept_language, None);

    // Always show success message to prevent email enumeration
    let template = "password_reset_sent.jinja";
    let ftl_lang = bundle
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string());

    // Validate email format
    if !form.email.contains('@') || form.email.parse::<lettre::Address>().is_err() {
        let rendered = state
            .render(
                template,
                context! {
                    email => form.email,
                    ftl_lang
                },
            )
            .await?;
        return Ok(Html(rendered).into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find user by email
    let user = find_user_by_email(&mut tx, &form.email).await?;

    // If user exists and email is verified, send reset email
    if let Some(user) = user {
        if user.email_verified_at.is_some() && user.deleted_at.is_none() {
            // Create challenge and send email
            let _ =
                create_and_send_password_reset_email(&state, user.id, &form.email, &bundle).await;
        }
    }

    tx.commit().await?;

    // Always show the same success message
    let rendered = state
        .render(
            template,
            context! {
                email => form.email,
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct PasswordResetVerifyForm {
    pub token: Uuid,
    pub new_password: String,
    pub new_password_confirm: String,
}

pub async fn password_reset_verify(
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Form(form): Form<PasswordResetVerifyForm>,
) -> Result<impl IntoResponse, AppError> {
    let bundle = get_bundle(&accept_language, None);
    let ftl_lang = bundle
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string());

    // Validate passwords match
    if form.new_password != form.new_password_confirm {
        messages.error(safe_get_message(&bundle, "password-reset-error-mismatch"));
        let rendered = state
            .render(
                "password_reset_verify.jinja",
                context! {
                    token => form.token,
                    ftl_lang
                },
            )
            .await?;
        return Ok(Html(rendered).into_response());
    }

    // Validate password length
    if form.new_password.len() < 8 {
        messages.error(safe_get_message(
            &bundle,
            "account-change-password-error-too-short",
        ));
        let rendered = state
            .render(
                "password_reset_verify.jinja",
                context! {
                    token => form.token,
                    ftl_lang
                },
            )
            .await?;
        return Ok(Html(rendered).into_response());
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find challenge by token
    let challenge = find_password_reset_challenge_by_token(&mut tx, form.token).await?;

    if let Some(challenge) = challenge {
        // Update password
        let _ = update_password(&mut tx, challenge.user_id, form.new_password.clone()).await?;

        // Delete all password reset challenges for this user
        delete_password_reset_challenges_for_user(&mut tx, challenge.user_id).await?;

        tx.commit().await?;

        messages.success(safe_get_message(&bundle, "password-reset-success"));

        Ok(Redirect::to("/login").into_response())
    } else {
        messages.error(safe_get_message(
            &bundle,
            "password-reset-error-invalid-token",
        ));
        let rendered = state
            .render(
                "password_reset_verify.jinja",
                context! {
                    token => form.token,
                    ftl_lang
                },
            )
            .await?;
        Ok(Html(rendered).into_response())
    }
}

pub async fn password_reset_verify_page(
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<TokenQuery>,
) -> Result<Html<String>, AppError> {
    let rendered = state
        .render(
            "password_reset_verify.jinja",
            context! {
                token => query.token.map(|t| t.to_string()),
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<Uuid>,
}

// Helper function to create password reset challenge and send email
async fn create_and_send_password_reset_email(
    state: &AppState,
    user_id: uuid::Uuid,
    email: &str,
    bundle: &fluent::bundle::FluentBundle<&FluentResource, IntlLangMemoizer>,
) -> Result<PasswordResetChallenge, String> {
    let db = &state.db_pool;
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;

    // Generate UUID token for magic link
    let token = Uuid::new_v4();

    let expires_at =
        Utc::now() + TimeDelta::try_seconds(60 * 15).expect("15 minutes is a valid duration"); // 15 minutes

    let password_reset_challenge =
        create_password_reset_challenge(&mut tx, user_id, email, token, expires_at)
            .await
            .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;

    // Send email
    let from_address = safe_get_message(bundle, "email-from-address");
    let email_message = Message::builder()
        .from(
            from_address
                .parse()
                .map_err(|e: lettre::address::AddressError| e.to_string())?,
        )
        .to(email
            .parse()
            .map_err(|e: lettre::address::AddressError| e.to_string())?)
        .subject(safe_get_message(bundle, "password-reset-email-subject"))
        .body(format!(
            "{}\n\nhttps://{}/password-reset/verify?token={}",
            safe_get_message(bundle, "password-reset-email-body"),
            state.config.domain,
            token
        ))
        .map_err(|e| format!("Failed to build email message: {}", e))?;

    let mailer = SmtpTransport::relay(&state.config.smtp_host)
        .map_err(|e| format!("Failed to create SMTP transport: {}", e))?
        .credentials(SmtpCredentials::new(
            state.config.smtp_user.clone(),
            state.config.smtp_password.clone(),
        ))
        .build();

    mailer.send(&email_message).map_err(|e| e.to_string())?;

    Ok(password_reset_challenge)
}
