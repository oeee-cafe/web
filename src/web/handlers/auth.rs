use crate::app_error::AppError;
use crate::models::device::delete_user_device_by_token;
use crate::models::user::{
    create_user, update_user_preferred_language, AuthSession, Credentials, UserDraft,
};
use crate::web::handlers::identity::{link_pending_identity, pending_provider_name};
use crate::web::handlers::{
    detect_preferred_language, get_bundle, safe_format_message, safe_get_message, ExtractFtlLang,
};
use crate::web::state::AppState;
use axum::extract::Query;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, Form};
use axum_messages::Messages;
use fluent::{FluentArgs, FluentValue};
use minijinja::context;
use serde::Deserialize;
use tower_sessions::Session;

use super::ExtractAcceptLanguage;

// This allows us to extract the "next" field from the query string. We use this
// to redirect after log in.
#[derive(Debug, Deserialize)]
pub struct NextUrl {
    next: Option<String>,
}

pub async fn signup(
    messages: Messages,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(NextUrl { next }): Query<NextUrl>,
    State(state): State<crate::web::state::AppState>,
) -> Result<impl IntoResponse, AppError> {
    let template: minijinja::Template<'_, '_> = state.env.get_template("signup.jinja")?;

    let rendered: String = template.render(context! {
        messages => messages.into_iter().collect::<Vec<_>>(),
        next => next,
        steam_enabled => state.config.steam.is_some(),
        apple_enabled => state.config.apple.is_some(),
        google_enabled => state.config.google.is_some(),
        ftl_lang
    })?;

    Ok(Html(rendered))
}

#[derive(Deserialize)]
pub struct CreateUserForm {
    login_name: String,
    password: String,
    password_confirm: String,
    display_name: String,
    next: Option<String>,
    /// The Community Guidelines and Privacy Policy box. A checkbox that is
    /// not ticked is not sent at all, so it is its presence that agrees.
    agree: Option<String>,
}

pub async fn do_signup(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Form(form): Form<CreateUserForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);

    // Checked here as well as by the box's `required`: a form posted without
    // the page, or from a page cached before the box existed, must not make
    // an account nobody agreed for.
    if form.agree.is_none() {
        messages.error(safe_get_message(&bundle, "signup-agree-required"));
        let back = match form.next.as_deref() {
            Some(next) => format!("/signup?next={}", urlencoding::encode(next)),
            None => "/signup".to_string(),
        };
        return Ok(Redirect::to(&back).into_response());
    }

    if form.password != form.password_confirm {
        messages.error(safe_get_message(
            &bundle,
            "account-change-password-error-mismatch",
        ));
        return Ok(Redirect::to("/signup").into_response());
    }

    let user_draft = UserDraft::new(form.login_name.clone(), form.password, form.display_name)?;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Check if login_name conflicts with any community slug
    if crate::models::user::login_name_conflicts_with_community(&mut tx, &form.login_name).await? {
        messages.error(safe_get_message(&bundle, "login-name-conflict-error"));
        return Ok(Redirect::to("/signup").into_response());
    }

    let user = create_user(&mut tx, user_draft, &state.config).await?;

    // Auto-set language preference from browser if it matches a supported language
    if let Some(lang) = detect_preferred_language(&accept_language) {
        update_user_preferred_language(&mut tx, user.id, Some(lang)).await?;
    }

    tx.commit().await?;

    if auth_session.login(&user).await.is_err() {
        return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    link_pending_identity(&session, &state, &messages, &bundle, &user).await?;

    let mut args = FluentArgs::new();
    args.set("name", FluentValue::from(user.display_name.clone()));
    messages.success(safe_format_message(&bundle, "welcome", Some(&args)));

    if let Some(ref next) = form.next {
        Ok(Redirect::to(next).into_response())
    } else {
        Ok(Redirect::to("/").into_response())
    }
}

pub async fn login(
    messages: Messages,
    session: Session,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Query(NextUrl { next }): Query<NextUrl>,
    State(state): State<crate::web::state::AppState>,
) -> Result<impl IntoResponse, AppError> {
    let template: minijinja::Template<'_, '_> = state.env.get_template("login.jinja")?;

    let collected_messages: Vec<axum_messages::Message> = messages.into_iter().collect();

    let rendered: String = template.render(context! {
        messages => collected_messages,
        next => next,
        // A provider's account waiting for this sign-in to be linked to.
        linking_provider => pending_provider_name(&session).await,
        steam_enabled => state.config.steam.is_some(),
        apple_enabled => state.config.apple.is_some(),
        google_enabled => state.config.google.is_some(),
        ftl_lang
    })?;

    Ok(Html(rendered))
}

pub async fn do_login(
    mut auth_session: AuthSession,
    session: Session,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    messages: Messages,
    State(state): State<AppState>,
    Form(creds): Form<Credentials>,
) -> impl IntoResponse {
    let user_preferred_language = auth_session
        .user
        .clone()
        .map(|u| u.preferred_language)
        .unwrap_or_else(|| None);
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let user = match auth_session.authenticate(creds.clone()).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            messages.error(safe_get_message(&bundle, "message-incorrect-credentials"));

            let mut login_url = "/login".to_string();
            if let Some(next) = creds.next {
                login_url = format!("{}?next={}", login_url, next);
            };

            return Redirect::to(&login_url).into_response();
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Auto-set language preference from browser if not already set
    if user.preferred_language.is_none() {
        if let Some(lang) = detect_preferred_language(&accept_language) {
            let db = &state.db_pool;
            if let Ok(mut tx) = db.begin().await {
                if update_user_preferred_language(&mut tx, user.id, Some(lang))
                    .await
                    .is_ok()
                {
                    let _ = tx.commit().await;
                }
            }
        }
    }

    if auth_session.login(&user).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if link_pending_identity(&session, &state, &messages, &bundle, &user)
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let mut args = FluentArgs::new();
    args.set("name", FluentValue::from(user.display_name.clone()));
    messages.success(safe_format_message(&bundle, "welcome", Some(&args)));

    if let Some(ref next) = creds.next {
        Redirect::to(next)
    } else {
        Redirect::to("/")
    }
    .into_response()
}

#[derive(Debug)]
pub enum LoginError {
    UserNotFound,
    PasswordNotMatch,
}

/// The cookie naming the push token registered from this web view (POST
/// /devices sets it, devices.rs), so that signing out also stops that
/// device's notifications. No app deletes its device itself, so this is the
/// only way one is dropped, beyond the push service forgetting a token APNs
/// or FCM has refused.
pub(crate) const DEVICE_COOKIE: &str = "oeee_device";

fn device_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == DEVICE_COOKIE)
        .map(|(_, token)| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

pub async fn do_logout(
    mut auth_session: AuthSession,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let (Some(user), Some(token)) = (auth_session.user.as_ref(), device_cookie(&headers)) {
        if let Ok(mut tx) = state.db_pool.begin().await {
            let _ = delete_user_device_by_token(&mut tx, user.id, &token).await;
            let _ = tx.commit().await;
        }
    }
    match auth_session.logout().await {
        Ok(_) => Redirect::to("/").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::COOKIE, HeaderMap, HeaderValue};

    #[test]
    fn the_device_cookie_is_found_among_the_others() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("id=abc; oeee_device=tok123; theme=dark"));
        assert_eq!(device_cookie(&headers).as_deref(), Some("tok123"));
    }

    #[test]
    fn no_device_cookie_is_none() {
        let mut headers = HeaderMap::new();
        assert_eq!(device_cookie(&headers), None);
        headers.insert(COOKIE, HeaderValue::from_static("oeee_device=; x_oeee_device=nope"));
        assert_eq!(device_cookie(&headers), None);
    }
}
