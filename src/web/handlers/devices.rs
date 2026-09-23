use crate::app_error::AppError;
use crate::models::device::{register_device, Device, PlatformType};
use crate::models::user::AuthSession;
use crate::web::state::AppState;
use axum::{extract::State, http::header, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterDeviceRequest {
    pub device_token: String,
    pub platform: PlatformType,
}

#[derive(Debug, Serialize)]
pub struct DeviceResponse {
    pub id: String,
    pub device_token: String,
    pub platform: PlatformType,
    pub created_at: String,
}

impl From<Device> for DeviceResponse {
    fn from(device: Device) -> Self {
        Self {
            id: device.id.to_string(),
            device_token: device.device_token,
            platform: device.platform,
            created_at: device.created_at.to_rfc3339(),
        }
    }
}

/// How long the device cookie lasts: 400 days, the longest a browser keeps
/// one. A token outlives any session, and registering again renews it.
const DEVICE_COOKIE_MAX_AGE: u64 = 400 * 24 * 60 * 60;

/// The cookie that names this device to the site's sign-out
/// (auth.rs, `do_logout`). HttpOnly: nothing in the page needs to read it.
fn device_cookie(token: &str) -> String {
    format!(
        "{}={token}; Path=/; Max-Age={DEVICE_COOKIE_MAX_AGE}; Secure; HttpOnly; SameSite=Lax",
        super::auth::DEVICE_COOKIE
    )
}

/// Register a device for the authenticated user.
///
/// Called by the page, in an app, with the token the app handed it
/// (oeeeApp.pushToken, app_bridge.jinja), so it carries the page's own
/// session. The same token again is an update, and the account signed in now
/// takes it over. The answer names the device to the web view's sign-out, so
/// the app neither makes this request nor sets that cookie.
pub async fn register_device_handler(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Json(payload): Json<RegisterDeviceRequest>,
) -> Result<impl IntoResponse, AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    let mut tx = state.db_pool.begin().await?;

    let device = register_device(&mut tx, user.id, payload.device_token, payload.platform).await?;

    tx.commit().await?;

    let cookie = device_cookie(&device.device_token);
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(DeviceResponse::from(device)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the sign-out reads back (auth.rs), set where the page's scripts
    /// cannot reach it and kept as long as a browser keeps anything.
    #[test]
    fn the_device_cookie_names_the_token_to_the_sign_out() {
        let cookie = device_cookie("abc123");
        assert!(cookie.starts_with("oeee_device=abc123;"));
        for part in ["Path=/", "Max-Age=34560000", "Secure", "HttpOnly", "SameSite=Lax"] {
            assert!(cookie.contains(part), "{part} in {cookie}");
        }
    }
}
