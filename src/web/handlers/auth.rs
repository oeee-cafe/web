use crate::app_error::{error_codes, AppError};
use crate::models::device::delete_device_by_token;
use crate::models::user::{
    create_user, update_user_preferred_language, AuthSession, Credentials, Language, UserDraft,
};
use crate::web::handlers::identity::{link_pending_identity, pending_provider_name};
use crate::web::handlers::{
    detect_preferred_language, get_bundle, safe_format_message, safe_get_message, ExtractFtlLang,
};
use crate::web::responses::ErrorResponse;
use crate::web::state::AppState;
use axum::extract::Query;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, response::Json, Form};
use axum_messages::Messages;
use fluent::{FluentArgs, FluentValue};
use minijinja::context;
use serde::{Deserialize, Serialize};
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

pub async fn do_logout(mut auth_session: AuthSession) -> impl IntoResponse {
    match auth_session.logout().await {
        Ok(_) => Redirect::to("/").into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// JSON API types
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub login_name: String,
    pub password: String,
    pub preferred_language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user: UserInfo,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub login_name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_verified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_language: Option<Language>,
}

#[derive(Debug, Deserialize)]
pub struct LogoutRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_token: Option<String>,
}

// LogoutResponse is now empty - success is indicated by 204 No Content

#[derive(Debug, Deserialize)]
pub struct SignupRequest {
    pub login_name: String,
    pub password: String,
    pub display_name: String,
    pub preferred_language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SignupResponse {
    pub user: UserInfo,
}

// JSON API endpoint for login
pub async fn api_login(
    mut auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    // Create credentials from JSON request
    let creds = Credentials {
        login_name: req.login_name,
        password: req.password,
        next: None,
    };

    // Authenticate user
    let user = match auth_session.authenticate(creds).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse::new(
                    error_codes::INVALID_CREDENTIALS,
                    "Invalid credentials",
                )),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Authentication error",
                )),
            )
                .into_response();
        }
    };

    // Auto-set language preference if not already set
    if user.preferred_language.is_none() {
        // Determine language: explicit request field takes priority over Accept-Language
        let lang_to_set = req.preferred_language.as_deref().and_then(|lang_str| {
            match lang_str {
                "ko" => Some(Language::Ko),
                "ja" => Some(Language::Ja),
                "en" => Some(Language::En),
                "zh" => Some(Language::Zh),
                _ => None,
            }
        }).or_else(|| detect_preferred_language(&accept_language));

        if let Some(lang) = lang_to_set {
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

    // Login user (sets session cookie)
    if auth_session.login(&user).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                error_codes::INTERNAL_ERROR,
                "Login error",
            )),
        )
            .into_response();
    }

    // Return success with user info
    (
        StatusCode::OK,
        Json(LoginResponse {
            user: UserInfo {
                id: user.id.to_string(),
                login_name: user.login_name,
                display_name: user.display_name,
                email: user.email,
                email_verified_at: user.email_verified_at.map(|dt| dt.to_rfc3339()),
                banner_id: user.banner_id.map(|id| id.to_string()),
                preferred_language: user.preferred_language,
            },
        }),
    )
        .into_response()
}

// JSON API endpoint for logout
pub async fn api_logout(
    mut auth_session: AuthSession,
    State(state): State<AppState>,
    Json(request): Json<LogoutRequest>,
) -> impl IntoResponse {
    // Delete the push token for this device if provided
    if let Some(device_token) = request.device_token {
        let db_pool = &state.db_pool;
        if let Ok(mut tx) = db_pool.begin().await {
            let _ = delete_device_by_token(&mut tx, device_token).await;
            let _ = tx.commit().await;
        }
    }

    match auth_session.logout().await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                error_codes::INTERNAL_ERROR,
                "Logout error",
            )),
        )
            .into_response(),
    }
}

// JSON API endpoint to get current user info
pub async fn api_me(auth_session: AuthSession) -> impl IntoResponse {
    match auth_session.user {
        Some(user) => (
            StatusCode::OK,
            Json(UserInfo {
                id: user.id.to_string(),
                login_name: user.login_name,
                display_name: user.display_name,
                email: user.email,
                email_verified_at: user.email_verified_at.map(|dt| dt.to_rfc3339()),
                banner_id: user.banner_id.map(|id| id.to_string()),
                preferred_language: user.preferred_language,
            }),
        )
            .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse::new(
                error_codes::UNAUTHORIZED,
                "Not authenticated",
            )),
        )
            .into_response(),
    }
}

// JSON API endpoint for signup
pub async fn api_signup(
    mut auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Json(req): Json<SignupRequest>,
) -> impl IntoResponse {
    // Create user draft
    let login_name = req.login_name.clone();
    let user_draft = match UserDraft::new(req.login_name, req.password, req.display_name) {
        Ok(draft) => draft,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(
                    error_codes::VALIDATION_ERROR,
                    e.to_string(),
                )),
            )
                .into_response();
        }
    };

    // Create user in database
    let db = &state.db_pool;
    let mut tx = match db.begin().await {
        Ok(tx) => tx,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Database error",
                )),
            )
                .into_response();
        }
    };

    // Check if login_name conflicts with any community slug
    match crate::models::user::login_name_conflicts_with_community(&mut tx, &login_name).await {
        Ok(true) => {
            return (
                StatusCode::CONFLICT,
                Json(ErrorResponse::new(
                    error_codes::CONFLICT,
                    "Login name conflicts with an existing community",
                )),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Database error",
                )),
            )
                .into_response();
        }
        Ok(false) => {}
    }

    let user = match create_user(&mut tx, user_draft, &state.config).await {
        Ok(user) => user,
        Err(e) => {
            // Check if this is a unique constraint violation for login_name
            let (error_code, error_message) = if let Some(db_err) = e.downcast_ref::<sqlx::Error>() {
                match db_err {
                    sqlx::Error::Database(db_error) => {
                        // PostgreSQL error code 23505 is unique_violation
                        if db_error.code().as_deref() == Some("23505") {
                            // Check if the error message mentions login_name
                            if db_error.message().contains("login_name") {
                                (
                                    error_codes::USERNAME_ALREADY_EXISTS,
                                    "This login name is already taken. Please choose a different one.",
                                )
                            } else {
                                (
                                    error_codes::CONFLICT,
                                    "A user with this information already exists.",
                                )
                            }
                        } else {
                            (error_codes::INTERNAL_ERROR, "Failed to create user.")
                        }
                    }
                    _ => (error_codes::INTERNAL_ERROR, "Failed to create user."),
                }
            } else {
                (error_codes::VALIDATION_ERROR, "Failed to create user.")
            };

            return (
                StatusCode::CONFLICT,
                Json(ErrorResponse::new(error_code, error_message)),
            )
                .into_response();
        }
    };

    // Determine language: explicit request field takes priority over Accept-Language
    let lang_to_set = req.preferred_language.as_deref().and_then(|lang_str| {
        match lang_str {
            "ko" => Some(Language::Ko),
            "ja" => Some(Language::Ja),
            "en" => Some(Language::En),
            "zh" => Some(Language::Zh),
            _ => None,
        }
    }).or_else(|| detect_preferred_language(&accept_language));

    if let Some(lang) = lang_to_set {
        if update_user_preferred_language(&mut tx, user.id, Some(lang))
            .await
            .is_err()
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    error_codes::INTERNAL_ERROR,
                    "Database error",
                )),
            )
                .into_response();
        }
    }

    if tx.commit().await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                error_codes::INTERNAL_ERROR,
                "Database error",
            )),
        )
            .into_response();
    }

    // Auto-login user (sets session cookie)
    if auth_session.login(&user).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                error_codes::INTERNAL_ERROR,
                "Login error",
            )),
        )
            .into_response();
    }

    // Return success with user info
    (
        StatusCode::CREATED,
        Json(SignupResponse {
            user: UserInfo {
                id: user.id.to_string(),
                login_name: user.login_name,
                display_name: user.display_name,
                email: user.email,
                email_verified_at: user.email_verified_at.map(|dt| dt.to_rfc3339()),
                banner_id: user.banner_id.map(|id| id.to_string()),
                preferred_language: user.preferred_language,
            },
        }),
    )
        .into_response()
}
