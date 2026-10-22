//! Changing an account from its staff page.

use crate::app_error::AppError;
use crate::models::admin::{find_user_detail, set_user_email};
use crate::web::handlers::AdminUser;
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::Form;
use minijinja::context;
use minijinja::value::Serde;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct SetEmailForm {
    /// Empty takes the address off.
    pub email: String,
    /// The checkbox, absent when it is left unticked.
    #[serde(default)]
    pub verified: bool,
}

/// POST /admin/users/:login_name/email — set the account's address without
/// the code the account page mails (`models::admin::set_user_email`).
/// Answers with the panel again, carrying what went wrong if anything did.
pub async fn admin_set_user_email(
    admin: AdminUser,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Form(form): Form<SetEmailForm>,
) -> Result<Html<String>, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let user = find_user_detail(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;
    let email = form.email.trim();

    let error = if user.deleted_at.is_some() {
        Some("A deleted account has no address to set.".to_string())
    } else if !email.is_empty() && email.parse::<lettre::Address>().is_err() {
        Some(format!("“{email}” is not an email address."))
    } else {
        let email = (!email.is_empty()).then_some(email);
        set_user_email(&mut tx, user.id, email, form.verified)
            .await?
            .err()
            .map(|holder| format!("@{holder} already has that address."))
    };

    // Re-read so the panel shows what landed, verified_at included.
    let user = match error {
        Some(_) => user,
        None => {
            let user = find_user_detail(&mut tx, &login_name)
                .await?
                .ok_or_else(|| AppError::NotFound("User".to_string()))?;
            tx.commit().await?;
            crate::staff_webhook::notify(crate::staff_webhook::Event::EmailSet {
                login_name: user.login_name.clone(),
                by: admin.0.login_name.to_string(),
                removed: user.email.is_none(),
                verified: user.email_verified_at.is_some(),
            });
            user
        }
    };

    let rendered = state
        .render(
            "admin/user_email_panel.jinja",
            context! {
                user => Serde(user),
                // What was typed stays in the field when it was refused.
                email => error.as_ref().map(|_| email),
                saved => error.is_none(),
                error,
            },
        )
        .await?;

    Ok(Html(rendered))
}
