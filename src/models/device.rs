use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction, Type};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
#[sqlx(type_name = "platform_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PlatformType {
    Ios,
    Android,
    /// The Mac app. Its tokens are APNs's, as the phones' are
    /// (src/push/mod.rs); it said "ios" before it had a name of its own.
    Macos,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub device_token: String,
    pub platform: PlatformType,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Register or update a device for a user
pub async fn register_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    device_token: String,
    platform: PlatformType,
) -> Result<Device> {
    let device = sqlx::query_as!(
        Device,
        r#"
        INSERT INTO devices (user_id, device_token, platform)
        VALUES ($1, $2, $3)
        ON CONFLICT (device_token, platform)
        DO UPDATE SET
            user_id = EXCLUDED.user_id,
            updated_at = CURRENT_TIMESTAMP
        RETURNING
            id,
            user_id,
            device_token,
            platform as "platform: PlatformType",
            created_at,
            updated_at
        "#,
        user_id,
        device_token,
        platform as PlatformType,
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(device)
}

/// Get all devices for a user
pub async fn get_user_devices(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<Device>> {
    let devices = sqlx::query_as!(
        Device,
        r#"
        SELECT
            id,
            user_id,
            device_token,
            platform as "platform: PlatformType",
            created_at,
            updated_at
        FROM devices
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#,
        user_id,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(devices)
}

/// Delete an invalid device (called when push fails)
pub async fn delete_invalid_device(
    tx: &mut Transaction<'_, Postgres>,
    device_token: String,
    platform: PlatformType,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        DELETE FROM devices
        WHERE device_token = $1 AND platform = $2
        "#,
        device_token,
        platform as PlatformType,
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Delete one of a user's devices, and only theirs: the token comes from a
/// cookie, which is no proof that the device was ever registered to them.
pub async fn delete_user_device_by_token(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    device_token: &str,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        DELETE FROM devices
        WHERE user_id = $1 AND device_token = $2
        "#,
        user_id,
        device_token,
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Get devices by platform for a user
pub async fn get_user_devices_by_platform(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    platform: PlatformType,
) -> Result<Vec<Device>> {
    let devices = sqlx::query_as!(
        Device,
        r#"
        SELECT
            id,
            user_id,
            device_token,
            platform as "platform: PlatformType",
            created_at,
            updated_at
        FROM devices
        WHERE user_id = $1 AND platform = $2
        ORDER BY created_at DESC
        "#,
        user_id,
        platform as PlatformType,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(devices)
}
