use crate::app_error::AppError;
use crate::models::email_verification_challenge::{
    create_email_verification_challenge, find_email_verification_challenge_by_id,
    EmailVerificationChallenge,
};
use crate::models::identity::list_identities_for_user;
use crate::models::supporter::{
    current_year, mark_for, set_mark, set_show_in_credits, shows_in_credits, standings, Store,
};
use crate::models::user::{
    delete_user_with_activity, find_user_by_id, update_password, update_user_email_verified_at,
    update_user_preferred_language, update_user_show_sensitive_content, update_user_with_activity,
    AuthSession, DeleteConfirmation,
};
use crate::web::context::CommonContext;
use crate::web::handlers::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::language::{language_set_cookie, parse_language};
use crate::web::state::AppState;
use axum::response::{IntoResponse, Redirect};
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::Html,
    Form,
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
    // The platforms this year's pack was bought on, which is what there is
    // to choose a mark between. A year that has passed is on their profile
    // and not here. `None` for anyone not supporting this year, who has no
    // credits to be in and no mark to wear.
    let mut supporter_platforms: Vec<String> = match auth_session.user.as_ref() {
        Some(user) => standings(&mut tx, user.id)
            .await?
            .into_iter()
            .filter(|standing| standing.year == current_year())
            .map(|standing| standing.store)
            .collect(),
        None => Vec::new(),
    };
    // Two years bought on the same platform are one platform to choose.
    supporter_platforms.sort();
    supporter_platforms.dedup();
    let (show_in_credits, worn_mark) = match auth_session.user.as_ref() {
        Some(user) if !supporter_platforms.is_empty() => (
            Some(shows_in_credits(&mut tx, user.id).await?),
            mark_for(&mut tx, user.id).await?,
        ),
        _ => (None, None),
    };

    let languages = vec![
        ("ko", "한국어"),
        ("ja", "日本語"),
        ("en", "English"),
        ("zh", "中文"),
    ];
    let rendered = state
        .render(
            "account.jinja",
            context! {
                current_user => auth_session.user,
                languages,
                steam_linked => identities.iter().any(|i| i.provider == "steam"),
                apple_linked => identities.iter().any(|i| i.provider == "apple"),
                google_linked => identities.iter().any(|i| i.provider == "google"),
                identities,
                has_password,
                show_in_credits,
                supporter_platforms,
                // Not `supporter_mark`: account.jinja imports a macro by that name,
                // and an imported name wins over a context one.
                worn_mark,
                steam_enabled => state.config.steam.is_some(),
                apple_enabled => state.config.apple.is_some(),
                google_enabled => state.config.google.is_some(),
                draft_post_count => common_ctx.draft_post_count,
                unread_notification_count => common_ctx.unread_notification_count,
                messages => messages.into_iter().collect::<Vec<_>>(),
                ftl_lang
            },
        )
        .await?;

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
    let language = parse_language(form.language.as_deref());
    let _ = update_user_preferred_language(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
        language.clone(),
    )
    .await;
    let _ = tx.commit().await;

    // The toolbar's choice is kept in a cookie too, for after signing out;
    // left alone, it would go on choosing after "Auto" was picked here.
    let mut response = Redirect::to("/account").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        language_set_cookie(language.as_ref(), state.config.env == "production"),
    );
    Ok(response)
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
pub struct SupporterForm {
    pub show_in_credits: Option<String>,
    /// Which store's mark to wear: a store's name, or empty for the
    /// default. Absent -- which is what a page rendered by the other colour
    /// mid-deploy posts -- leaves the choice alone rather than clearing a
    /// mark it never showed.
    pub mark: Option<String>,
}

/// What a supporter is asked: whether they are thanked by name on /about,
/// and which store's mark goes beside their name. Their mark stays
/// either way; a name that is not a store is ignored.
pub async fn save_supporter_settings(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Form(form): Form<SupporterForm>,
) -> Result<impl IntoResponse, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let mut tx = state.db_pool.begin().await?;
    set_show_in_credits(
        &mut tx,
        user.id,
        form.show_in_credits.as_deref() == Some("on"),
    )
    .await?;
    match form.mark.as_deref() {
        Some("") => set_mark(&mut tx, user.id, None).await?,
        Some(name) => {
            if let Some(store) = Store::parse(name) {
                set_mark(&mut tx, user.id, Some(store)).await?;
            }
        }
        None => {}
    }
    tx.commit().await?;
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

    let template = "email_verify.jinja";
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
        let rendered = state.render(template, context! {
            challenge_id => challenge.id,
            email => challenge.email,
            message => safe_get_message(&bundle, "account-change-email-error-token-mismatch"),
            success => false,
            ftl_lang
        }).await?;

        return Ok(Html(rendered).into_response());
    }

    if challenge.expires_at < now {
        let ftl_lang = bundle
            .locales
            .first()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "en".to_string());
        let rendered = state.render(template, context! {
            challenge_id => challenge.id,
            email => challenge.email,
            message => safe_get_message(&bundle, "account-change-email-error-token-expired"),
            success => false,
            ftl_lang
        }).await?;

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
    let rendered = state
        .render(
            template,
            context! {
                challenge_id => challenge.id,
                email => challenge.email,
                message => safe_get_message(&bundle, "account-change-email-success"),
                success => true,
                ftl_lang
            },
        )
        .await?;

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
        return Ok(Html(state.render("email_edit.jinja", context! {
            current_user => auth_session.user,
            message => safe_get_message(&bundle, "account-change-email-error-already-verified"),
            ftl_lang,
        }).await?)
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

    let ftl_lang = bundle
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string());

    let rendered = state
        .render(
            "email_verify.jinja",
            context! {
                challenge_id => email_verification_challenge.id,
                email => form.email,
                ftl_lang,
            },
        )
        .await?;

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
