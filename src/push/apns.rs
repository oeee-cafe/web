use apns_h2::{
    request::payload::Payload, Client, ClientConfig, DefaultNotificationBuilder, Endpoint,
    Error as A2Error, ErrorReason, NotificationBuilder, NotificationOptions, PushType,
};
use std::fs::File;
use std::sync::Arc;

use super::PushError;

#[derive(Clone)]
pub struct ApnsClient {
    client: Arc<Client>,
    topic: String,
}

impl ApnsClient {
    pub fn new(
        key_path: &str,
        key_id: &str,
        team_id: &str,
        environment: &str,
        topic: &str,
    ) -> Result<Self, anyhow::Error> {
        let mut key_file = File::open(key_path)?;

        let endpoint = match environment {
            "production" => Endpoint::Production,
            _ => Endpoint::Sandbox,
        };

        let config = ClientConfig::new(endpoint);
        let client = Client::token(&mut key_file, key_id, team_id, config)?;

        Ok(Self {
            client: Arc::new(client),
            topic: topic.to_string(),
        })
    }

    pub async fn send_notification(
        &self,
        device_token: &str,
        title: &str,
        body: &str,
        badge: Option<u32>,
        data: Option<serde_json::Value>,
    ) -> Result<(), PushError> {
        let mut builder = DefaultNotificationBuilder::new()
            .title(title)
            .body(body)
            .sound("default");

        if let Some(badge_count) = badge {
            builder = builder.badge(badge_count);
        }

        // Build the notification payload with topic (bundle ID)
        let options = NotificationOptions {
            apns_topic: Some(&self.topic),
            ..Default::default()
        };
        let mut payload = builder.build(device_token, options);

        // Add custom data if provided
        if let Some(ref custom_data) = data {
            if let Some(data_obj) = custom_data.as_object() {
                for (key, value) in data_obj {
                    payload.add_custom_data(key, value).map_err(|e| {
                        PushError::Other(anyhow::anyhow!("Failed to add custom data: {:?}", e))
                    })?;
                }
            }
        }

        self.send(payload).await
    }

    /// Only the number on the app's icon: no banner, no sound, nothing in
    /// Notification Center. The system sets it whether or not the app is
    /// running.
    pub async fn send_badge(&self, device_token: &str, badge: u32) -> Result<(), PushError> {
        let options = NotificationOptions {
            apns_topic: Some(&self.topic),
            apns_push_type: Some(PushType::Alert),
            ..Default::default()
        };
        let payload = DefaultNotificationBuilder::new()
            .badge(badge)
            .build(device_token, options);
        self.send(payload).await
    }

    async fn send(&self, payload: Payload<'_>) -> Result<(), PushError> {
        let response = self.client.send(payload).await.map_err(|e| {
            // Check if it's a response error with an invalid token reason
            if let A2Error::ResponseError(ref resp) = e {
                if let Some(ref error_body) = resp.error {
                    match error_body.reason {
                        ErrorReason::Unregistered
                        | ErrorReason::BadDeviceToken
                        | ErrorReason::DeviceTokenNotForTopic => {
                            return PushError::InvalidToken;
                        }
                        _ => {}
                    }
                }
            }
            // For all other errors, don't capture backtrace
            PushError::Other(anyhow::Error::msg(format!("APNs send error: {:?}", e)))
        })?;

        // Check for errors in the response indicating invalid token
        if let Some(error) = response.error {
            match error.reason {
                ErrorReason::Unregistered
                | ErrorReason::BadDeviceToken
                | ErrorReason::DeviceTokenNotForTopic => {
                    return Err(PushError::InvalidToken);
                }
                _ => {
                    return Err(PushError::Other(
                        anyhow::anyhow!("APNs error: {:?}", error).context("APNs returned error"),
                    ));
                }
            }
        }

        Ok(())
    }
}
