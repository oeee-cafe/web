use google_fcm1::api::{AndroidConfig, AndroidNotification, Message, Notification};
use google_fcm1::hyper_rustls;
use google_fcm1::FirebaseCloudMessaging;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use std::sync::Arc;
use yup_oauth2::ServiceAccountAuthenticator;

use super::PushError;

#[derive(Clone)]
pub struct FcmClient {
    hub: Arc<FirebaseCloudMessaging<hyper_rustls::HttpsConnector<HttpConnector>>>,
    project_id: String,
}

impl FcmClient {
    pub async fn new(service_account_path: &str, project_id: &str) -> Result<Self, anyhow::Error> {
        // Read the service account key
        let service_account_key =
            yup_oauth2::read_service_account_key(service_account_path).await?;

        // Create an authenticator
        let auth = ServiceAccountAuthenticator::builder(service_account_key)
            .build()
            .await?;

        // Create the HTTP client with Hyper 1.0
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()?
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();

        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https);

        // Create the FCM hub
        let hub = FirebaseCloudMessaging::new(client, auth);

        Ok(Self {
            hub: Arc::new(hub),
            project_id: project_id.to_string(),
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
        // Build the notification
        let notification = Notification {
            title: Some(title.to_string()),
            body: Some(body.to_string()),
            ..Default::default()
        };

        // Build Android-specific configuration
        let mut android_notification = AndroidNotification {
            sound: Some("default".to_string()),
            ..Default::default()
        };

        // Add badge count if provided
        if let Some(badge_count) = badge {
            android_notification.notification_count = Some(badge_count as i32);
        }

        let android_config = AndroidConfig {
            priority: Some("high".to_string()),
            notification: Some(android_notification),
            ..Default::default()
        };

        // Convert custom data to HashMap<String, String>
        let mut data_map = None;
        if let Some(custom_data) = data {
            if let Some(obj) = custom_data.as_object() {
                let mut map = std::collections::HashMap::new();
                for (key, value) in obj {
                    // FCM V1 API requires all data values to be strings
                    if let Some(str_value) = value.as_str() {
                        map.insert(key.clone(), str_value.to_string());
                    } else {
                        map.insert(key.clone(), value.to_string());
                    }
                }
                if !map.is_empty() {
                    data_map = Some(map);
                }
            }
        }

        // Add badge to data payload for Android clients to read
        if let Some(badge_count) = badge {
            let mut map = data_map.unwrap_or_else(std::collections::HashMap::new);
            map.insert("badge".to_string(), badge_count.to_string());
            data_map = Some(map);
        }

        // Build the message
        let message = Message {
            token: Some(device_token.to_string()),
            notification: Some(notification),
            android: Some(android_config),
            data: data_map,
            ..Default::default()
        };

        // Create the send request
        let parent = format!("projects/{}", self.project_id);
        let req = google_fcm1::api::SendMessageRequest {
            message: Some(message),
            validate_only: Some(false),
        };

        self.send(req, &parent).await
    }

    /// Only the number on the bell, as data and nothing to show: the app
    /// takes its notifications down when it falls to nothing, since the
    /// launcher's badge is theirs (OeeeCafeMessagingService in
    /// oeee-cafe/android). Normal priority, since nothing is shown: FCM holds
    /// a high-priority message that shows nothing against the app.
    pub async fn send_badge(&self, device_token: &str, badge: u32) -> Result<(), PushError> {
        let message = Message {
            token: Some(device_token.to_string()),
            android: Some(AndroidConfig {
                priority: Some("normal".to_string()),
                ..Default::default()
            }),
            data: Some(std::collections::HashMap::from([(
                "badge".to_string(),
                badge.to_string(),
            )])),
            ..Default::default()
        };
        let parent = format!("projects/{}", self.project_id);
        let req = google_fcm1::api::SendMessageRequest {
            message: Some(message),
            validate_only: Some(false),
        };
        self.send(req, &parent).await
    }

    async fn send(
        &self,
        req: google_fcm1::api::SendMessageRequest,
        parent: &str,
    ) -> Result<(), PushError> {
        let result = self.hub.projects().messages_send(req, parent).doit().await;

        match result {
            Ok(_) => Ok(()),
            Err(google_fcm1::Error::BadRequest(ref body)) if token_is_dead(body) => {
                Err(PushError::InvalidToken)
            }
            Err(e) => Err(PushError::Other(anyhow::anyhow!("FCM error: {:?}", e))),
        }
    }
}

/// Whether FCM refused the send because of the token, and so the device can be
/// forgotten. Only two errors say that: `UNREGISTERED`, and `INVALID_ARGUMENT`
/// when the field it objects to is the token. `INVALID_ARGUMENT` on its own is
/// also what a malformed payload gets, and treating it as a dead token would
/// delete every Android device a bad message was tried on.
fn token_is_dead(body: &serde_json::Value) -> bool {
    let Some(details) = body["error"]["details"].as_array() else {
        return false;
    };
    details.iter().any(|detail| match detail["@type"].as_str() {
        Some("type.googleapis.com/google.firebase.fcm.v1.FcmError") => {
            detail["errorCode"] == "UNREGISTERED"
        }
        Some("type.googleapis.com/google.rpc.BadRequest") => detail["fieldViolations"]
            .as_array()
            .is_some_and(|violations| {
                violations
                    .iter()
                    .any(|violation| violation["field"] == "message.token")
            }),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::token_is_dead;
    use serde_json::json;

    #[test]
    fn an_unregistered_token_is_dead() {
        assert!(token_is_dead(&json!({"error": {
            "code": 404,
            "status": "NOT_FOUND",
            "details": [{
                "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                "errorCode": "UNREGISTERED",
            }],
        }})));
    }

    #[test]
    fn a_malformed_token_is_dead() {
        assert!(token_is_dead(&json!({"error": {
            "code": 400,
            "status": "INVALID_ARGUMENT",
            "details": [
                {
                    "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                    "errorCode": "INVALID_ARGUMENT",
                },
                {
                    "@type": "type.googleapis.com/google.rpc.BadRequest",
                    "fieldViolations": [{
                        "field": "message.token",
                        "description": "The registration token is not a valid FCM registration token",
                    }],
                },
            ],
        }})));
    }

    #[test]
    fn a_malformed_payload_leaves_the_token_alone() {
        assert!(!token_is_dead(&json!({"error": {
            "code": 400,
            "status": "INVALID_ARGUMENT",
            "details": [
                {
                    "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                    "errorCode": "INVALID_ARGUMENT",
                },
                {
                    "@type": "type.googleapis.com/google.rpc.BadRequest",
                    "fieldViolations": [{
                        "field": "message.android.notification.notification_count",
                        "description": "Invalid value",
                    }],
                },
            ],
        }})));
    }

    #[test]
    fn a_sender_mismatch_leaves_the_token_alone() {
        // Our credentials, not the device: forgetting on this would empty the
        // table the first time the service account was misconfigured.
        assert!(!token_is_dead(&json!({"error": {
            "code": 403,
            "status": "PERMISSION_DENIED",
            "details": [{
                "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                "errorCode": "SENDER_ID_MISMATCH",
            }],
        }})));
    }
}
