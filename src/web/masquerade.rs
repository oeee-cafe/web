//! Staff signed in as somebody else, to see the site as they see it and act
//! on their behalf.
//!
//! An admin starts one from the account's staff page
//! (`handlers::admin::admin_masquerade`). The session is then the account's,
//! as a sign-in would make it, with one more key beside axum-login's: who to
//! go back to. Every page in the site's chrome says so under the toolbar
//! (`CommonContext::masquerading_from`), with the button that goes back.
//!
//! What it does not reach is the account itself: its sign-ins, password,
//! address, deletion, devices and purchases ([`refused`]). Those are the
//! person's, and a masquerade that could change them could also keep the
//! account after it ended -- an admin's Google linked to it, an admin's
//! phone getting its notifications, an admin's Steam purchase on it.
//!
//! The browser's collaborative guest is not handed over either
//! (`guest::adopt`): the person at the keyboard is the admin, and their guest
//! went to their own account when they signed in.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::user::{find_user_by_id, AuthSession, User};
use crate::web::state::AppState;

/// The session key, beside axum-login's own.
const KEY: &str = "masquerade";

/// Who the session goes back to when the masquerade ends.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Masquerade {
    pub admin_id: Uuid,
    pub admin_login_name: String,
}

tokio::task_local! {
    /// The request's masquerade, set by [`layer`] for everything under it, so
    /// `CommonContext::build` can draw the banner without every one of its
    /// callers passing the session along.
    static CURRENT: Option<Masquerade>;
}

/// The masquerade the request being answered is in, if any. `None` outside
/// [`layer`], as in a test.
pub fn current() -> Option<Masquerade> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

async fn of(session: &Session) -> Option<Masquerade> {
    match session.get::<Masquerade>(KEY).await {
        Ok(masquerade) => masquerade,
        Err(e) => {
            tracing::warn!("Could not read the session's masquerade: {e}");
            None
        }
    }
}

/// Whether a request is one a masquerade does not make: anything that signs
/// in, links a sign-in, or changes the account rather than acting with it.
/// Reading the account page is fine; saving it is not.
pub fn refused(method: &Method, path: &str) -> bool {
    const ANY_METHOD: [&str; 6] = [
        "/auth/",
        "/api/auth",
        "/api/discord/",
        "/store/",
        "/password-reset",
        "/devices",
    ];
    const NOT_READING: [&str; 3] = ["/account", "/login", "/signup"];

    let under = |prefix: &str| {
        path == prefix.trim_end_matches('/')
            || (path.starts_with(prefix)
                && (prefix.ends_with('/') || path[prefix.len()..].starts_with('/')))
    };
    ANY_METHOD.iter().any(|prefix| under(prefix))
        || (method != Method::GET
            && method != Method::HEAD
            && NOT_READING.iter().any(|prefix| under(prefix)))
}

/// Puts the session's masquerade where [`current`] finds it, and turns away
/// what [`refused`] names. Inside the auth layer, which is what put the
/// session in the extensions.
pub async fn layer(req: Request, next: Next) -> Response {
    let masquerade = match req.extensions().get::<Session>() {
        Some(session) => of(session).await,
        None => None,
    };
    if masquerade.is_some() && refused(req.method(), req.uri().path()) {
        return AppError::BadRequest(
            "Not while signed in as somebody else: this is theirs to do.".to_string(),
        )
        .into_response();
    }
    CURRENT.scope(masquerade, next.run(req)).await
}

/// Signs `admin`'s session in as `target`, remembering the way back.
/// The caller has checked that `admin` may.
pub async fn begin(
    auth_session: &mut AuthSession,
    session: &Session,
    admin: &User,
    target: &User,
) -> Result<(), AppError> {
    session
        .insert(
            KEY,
            Masquerade {
                admin_id: admin.id,
                admin_login_name: admin.login_name.to_string(),
            },
        )
        .await
        .map_err(|e| AppError::Anyhow(e.into()))?;
    // axum-login only cycles the id for a session nobody is signed in to.
    // This is a different account now, and gets a different id.
    session
        .cycle_id()
        .await
        .map_err(|e| AppError::Anyhow(e.into()))?;
    auth_session
        .login(target)
        .await
        .map_err(|e| AppError::Anyhow(anyhow::anyhow!("{e}")))?;
    Ok(())
}

/// POST /masquerade/end — back to the admin who started it, on the staff page
/// of the account they were. Outside the login gate: the account may have
/// been deleted while it lasted, which signs the session out of it, and the
/// way back still has to work. An admin who is no longer one is signed out.
pub async fn end(
    mut auth_session: AuthSession,
    session: Session,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let Some(masquerade) = of(&session).await else {
        return Ok(Redirect::to("/").into_response());
    };
    let was = auth_session.user.as_ref().map(|u| u.login_name.to_string());

    let mut tx = state.db_pool.begin().await?;
    let admin = find_user_by_id(&mut tx, masquerade.admin_id)
        .await?
        .filter(|admin| admin.is_admin() && admin.deleted_at.is_none());
    tx.commit().await?;

    let Some(admin) = admin else {
        auth_session
            .logout()
            .await
            .map_err(|e| AppError::Anyhow(anyhow::anyhow!("{e}")))?;
        return Ok(Redirect::to("/").into_response());
    };

    session
        .remove::<Masquerade>(KEY)
        .await
        .map_err(|e| AppError::Anyhow(e.into()))?;
    session
        .cycle_id()
        .await
        .map_err(|e| AppError::Anyhow(e.into()))?;
    auth_session
        .login(&admin)
        .await
        .map_err(|e| AppError::Anyhow(anyhow::anyhow!("{e}")))?;

    let back = match was {
        Some(login_name) => format!("/admin/users/{}", urlencoding::encode(&login_name)),
        None => "/admin/users".to_string(),
    };
    Ok(Redirect::to(&back).into_response())
}

/// Whether the session is in a masquerade. For handlers outside [`layer`]'s
/// reach or that need to know before rendering anything, such as sign-out.
pub async fn is_on(session: &Session) -> bool {
    of(session).await.is_some()
}

#[cfg(test)]
mod tests {
    use super::refused;
    use axum::http::Method;

    #[test]
    fn the_account_is_read_but_not_changed() {
        assert!(!refused(&Method::GET, "/account"));
        assert!(refused(&Method::POST, "/account"));
        assert!(refused(&Method::POST, "/account/password"));
        assert!(refused(&Method::DELETE, "/account/delete"));
        assert!(refused(&Method::POST, "/account/identities/google/unlink"));
    }

    #[test]
    fn signing_in_and_what_lands_on_the_account_are_refused() {
        assert!(refused(&Method::GET, "/auth/google"));
        assert!(refused(&Method::POST, "/auth/handoff/start"));
        assert!(refused(&Method::POST, "/api/auth"));
        assert!(refused(&Method::POST, "/login"));
        assert!(refused(&Method::POST, "/signup"));
        assert!(refused(&Method::POST, "/devices"));
        assert!(refused(&Method::POST, "/store/steam/purchases"));
        assert!(refused(&Method::GET, "/password-reset/verify"));
    }

    #[test]
    fn acting_as_them_is_not() {
        assert!(!refused(&Method::POST, "/comments"));
        assert!(!refused(&Method::POST, "/draw/finish"));
        assert!(!refused(&Method::POST, "/logout"));
        assert!(!refused(&Method::GET, "/login"));
        // A prefix is a path segment, not a string.
        assert!(!refused(&Method::POST, "/accounts-of-something"));
        assert!(!refused(&Method::GET, "/storefront"));
        assert!(!refused(&Method::GET, "/supporter"));
    }
}
