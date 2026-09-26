pub mod apns;
pub mod fcm;

use crate::live::{Live, LiveEvent};
use crate::models::device::{delete_invalid_device, get_user_devices_by_platform, PlatformType};
use crate::models::notification::get_badge_count;
use crate::AppConfig;
use anyhow::Result;
use apns::ApnsClient;
use fcm::FcmClient;
use sqlx::PgPool;
use std::sync::Arc;

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
    /// The reader's open pages, which hear what their devices are sent: the
    /// bell's number, and a new notification's words (crate::live).
    live: Option<Live>,
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
            live: None,
        })
    }

    /// Tells the reader's open pages too, whenever their devices are told.
    pub fn with_live(mut self, live: Live) -> Self {
        self.live = Some(live);
        self
    }

    /// A push service with no transport configured. Every send becomes a no-op,
    /// which lets the server keep running when push credentials are broken.
    pub fn disabled(db_pool: PgPool) -> Self {
        Self {
            apns_client: None,
            fcm_client: None,
            db_pool,
            live: None,
        }
    }

    pub async fn send_notification_to_user(
        &self,
        user_id: uuid::Uuid,
        title: &str,
        body: &str,
        badge: Option<u32>,
        url: &str,
        mut data: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        // Pages first: they need no device and no database.
        if let Some(live) = &self.live {
            live.publish(LiveEvent::Notification {
                user_id,
                title: title.to_string(),
                body: body.to_string(),
                url: url.to_string(),
            });
            if let Some(count) = badge {
                live.publish(LiveEvent::Unread {
                    user_id,
                    count: i64::from(count),
                });
            }
        }

        // The page tapping it opens, a path on the site. Every push has one: the apps open
        // it and have nothing of their own to fall back on.
        data.insert("url".to_string(), serde_json::json!(url));
        let data = Some(serde_json::Value::Object(data));

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

    /// Sets the number on the user's devices to what the bell says now, in the
    /// background. For when it falls -- a notification read or deleted, an
    /// invitation answered or withdrawn -- which the pushes for new
    /// notifications, the only ones that carry it, never say: the icon kept the
    /// last push's number until the next one came.
    ///
    /// iPhones, iPads and Macs get the number itself, which the system puts on
    /// the icon. Android's badge is the app's notifications on show, so it is
    /// sent the number as data, and takes them down at nothing.
    ///
    /// The reader's open pages are told as well (crate::live), which is what
    /// takes the bell down in another tab when one is read here.
    pub fn refresh_badge(self: &Arc<Self>, user_id: uuid::Uuid) {
        let devices = self.apns_client.is_some() || self.fcm_client.is_some();
        if !devices && self.live.is_none() {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = this.send_badge_to_user(user_id, devices).await {
                tracing::warn!("Failed to refresh the badge for user {}: {:?}", user_id, e);
            }
        });
    }

    async fn send_badge_to_user(&self, user_id: uuid::Uuid, devices: bool) -> Result<()> {
        let mut tx = self.db_pool.begin().await?;
        let count = get_badge_count(&mut tx, user_id).await?;
        if let Some(live) = &self.live {
            live.publish(LiveEvent::Unread { user_id, count });
        }
        if !devices {
            return Ok(());
        }
        let badge = u32::try_from(count).unwrap_or(0);

        let mut platforms = Vec::new();
        if self.apns_client.is_some() {
            platforms.extend([PlatformType::Ios, PlatformType::Macos]);
        }
        if self.fcm_client.is_some() {
            platforms.push(PlatformType::Android);
        }
        for platform in platforms {
            let devices = get_user_devices_by_platform(&mut tx, user_id, platform.clone()).await?;
            for token in devices {
                let sent = match platform {
                    PlatformType::Android => match &self.fcm_client {
                        Some(fcm) => fcm.send_badge(&token.device_token, badge).await,
                        None => Ok(()),
                    },
                    PlatformType::Ios | PlatformType::Macos => match &self.apns_client {
                        Some(apns) => apns.send_badge(&token.device_token, badge).await,
                        None => Ok(()),
                    },
                };
                match sent {
                    Ok(_) => {}
                    Err(PushError::InvalidToken) => {
                        tracing::info!("Removing invalid push token: {}", token.device_token);
                        let _ = delete_invalid_device(
                            &mut tx,
                            token.device_token.clone(),
                            platform.clone(),
                        )
                        .await;
                    }
                    Err(PushError::Other(e)) => {
                        tracing::warn!(
                            "Failed to send the badge to token {}: {}",
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
