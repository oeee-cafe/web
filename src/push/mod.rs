pub mod apns;
pub mod fcm;

use crate::models::device::{delete_invalid_device, get_user_devices_by_platform, PlatformType};
use crate::AppConfig;
use anyhow::Result;
use apns::ApnsClient;
use fcm::FcmClient;
use sqlx::PgPool;

#[derive(Debug)]
pub enum PushError {
    InvalidToken,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for PushError {
    fn from(err: anyhow::Error) -> Self {
        PushError::Other(err)
    }
}

#[derive(Clone)]
pub struct PushService {
    apns_client: Option<ApnsClient>,
    fcm_client: Option<FcmClient>,
    db_pool: PgPool,
}

impl PushService {
    pub async fn new(config: &AppConfig, db_pool: PgPool) -> Result<Self> {
        let apns_client = if !config.apns_key_path.is_empty() {
            match ApnsClient::new(
                &config.apns_key_path,
                &config.apns_key_id,
                &config.apns_team_id,
                &config.apns_environment,
                &config.apns_topic,
            ) {
                Ok(client) => {
                    tracing::info!("APNs client initialized successfully");
                    Some(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize APNs client: {:?}", e);
                    None
                }
            }
        } else {
            tracing::warn!(
                "APNs configuration not provided, push notifications for iOS will not work"
            );
            None
        };

        let fcm_client = if !config.fcm_service_account_path.is_empty()
            && !config.fcm_project_id.is_empty()
        {
            match FcmClient::new(&config.fcm_service_account_path, &config.fcm_project_id).await {
                Ok(client) => {
                    tracing::info!("FCM client initialized successfully");
                    Some(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize FCM client: {:?}", e);
                    None
                }
            }
        } else {
            tracing::warn!(
                "FCM configuration not provided, push notifications for Android will not work"
            );
            None
        };

        Ok(Self {
            apns_client,
            fcm_client,
            db_pool,
        })
    }

    /// A push service with no transport configured. Every send becomes a no-op,
    /// which lets the server keep running when push credentials are broken.
    pub fn disabled(db_pool: PgPool) -> Self {
        Self {
            apns_client: None,
            fcm_client: None,
            db_pool,
        }
    }

    pub async fn send_notification_to_user(
        &self,
        user_id: uuid::Uuid,
        title: &str,
        body: &str,
        badge: Option<u32>,
        data: Option<serde_json::Value>,
    ) -> Result<()> {
        // Get user's tokens from database
        let mut tx = self.db_pool.begin().await?;

        // Send to the iPhones, iPads and Macs, all through APNs.
        if self.apns_client.is_some() {
            for platform in [PlatformType::Ios, PlatformType::Macos] {
                let devices =
                    get_user_devices_by_platform(&mut tx, user_id, platform.clone()).await?;
                for token in devices {
                    match self
                        .send_to_apns(&token.device_token, title, body, badge, data.clone())
                        .await
                    {
                        Ok(_) => {}
                        Err(PushError::InvalidToken) => {
                            tracing::info!("Removing invalid APNs token: {}", token.device_token);
                            let _ = delete_invalid_device(
                                &mut tx,
                                token.device_token.clone(),
                                platform.clone(),
                            )
                            .await;
                        }
                        Err(PushError::Other(e)) => {
                            tracing::warn!(
                                "Failed to send APNs notification to token {}: {}",
                                token.device_token,
                                e
                            );
                        }
                    }
                }
            }
        }

        // Send to Android devices
        if self.fcm_client.is_some() {
            let android_devices =
                get_user_devices_by_platform(&mut tx, user_id, PlatformType::Android).await?;
            for token in android_devices {
                match self
                    .send_to_fcm(&token.device_token, title, body, badge, data.clone())
                    .await
                {
                    Ok(_) => {}
                    Err(PushError::InvalidToken) => {
                        tracing::info!("Removing invalid FCM token: {}", token.device_token);
                        let _ = delete_invalid_device(
                            &mut tx,
                            token.device_token.clone(),
                            PlatformType::Android,
                        )
                        .await;
                    }
                    Err(PushError::Other(e)) => {
                        tracing::warn!(
                            "Failed to send FCM notification to token {}: {}",
                            token.device_token,
                            e
                        );
                    }
                }
            }
        }

        tx.commit().await?;
        Ok(())
    }

    async fn send_to_apns(
        &self,
        device_token: &str,
        title: &str,
        body: &str,
        badge: Option<u32>,
        data: Option<serde_json::Value>,
    ) -> Result<(), PushError> {
        if let Some(client) = &self.apns_client {
            client
                .send_notification(device_token, title, body, badge, data)
                .await?;
        }
        Ok(())
    }

    async fn send_to_fcm(
        &self,
        device_token: &str,
        title: &str,
        body: &str,
        badge: Option<u32>,
        data: Option<serde_json::Value>,
    ) -> Result<(), PushError> {
        if let Some(client) = &self.fcm_client {
            client
                .send_notification(device_token, title, body, badge, data)
                .await?;
        }
        Ok(())
    }
}
