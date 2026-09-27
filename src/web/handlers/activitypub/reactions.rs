//! Likes and emoji reactions, from other servers.

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::Activity;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::web::state::AppState;

use super::{actor_from_signature_deser, string_or_vec_deser};

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Like {
    pub actor: ObjectId<Actor>,
    #[serde(rename = "object")]
    pub object: Url,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: Url,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
}

#[async_trait::async_trait]
impl Activity for Like {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("=== RECEIVED LIKE ACTIVITY ===");
        tracing::info!("Actor: {}", self.actor.inner());
        tracing::info!("Object: {}", self.object);
        tracing::info!("Converting Like to ❤️ reaction");
        tracing::info!("================================");

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Dereference the actor
        let actor = self.actor.dereference(data).await?;
        let persisted_actor = Actor::create_or_update_actor(&mut tx, &actor).await?;

        // Parse post IRI to extract post_id
        let object_url = self.object.to_string();

        // Try to extract post ID from either URL format:
        // https://domain/@username/post-id or https://domain/ap/posts/post-id
        let user_post_prefix = format!("https://{}/@", data.app_data().config.domain);
        let ap_post_prefix = format!("https://{}/ap/posts/", data.app_data().config.domain);

        let post_id_str = if object_url.starts_with(&user_post_prefix) {
            // Extract from URLs like https://domain/@username/post-id
            let path_part = &object_url[user_post_prefix.len()..];
            path_part.find('/').map(|pos| &path_part[pos + 1..])
        } else if object_url.starts_with(&ap_post_prefix) {
            // Extract from URLs like https://domain/ap/posts/post-id
            Some(&object_url[ap_post_prefix.len()..])
        } else {
            None
        };

        if let Some(post_id_str) = post_id_str {
            if let Ok(post_id) = Uuid::parse_str(post_id_str) {
                // Verify post exists and get post author
                if let Some(post) = find_post_by_id(&mut tx, post_id).await? {
                    // Get post author's user_id
                    let post_author_user_id = post
                        .get("author_id")
                        .and_then(|id| id.as_ref())
                        .and_then(|id_str| Uuid::parse_str(id_str).ok());

                    // Create reaction using Like's IRI (for idempotency)
                    use crate::models::reaction::create_reaction_from_activitypub;
                    match create_reaction_from_activitypub(
                        &mut tx,
                        self.id.to_string(),
                        post_id,
                        persisted_actor.id,
                        "❤️".to_string(),
                    )
                    .await
                    {
                        Ok(reaction) => {
                            tracing::info!(
                                "Created ❤️ reaction from Like activity for post {}",
                                post_id
                            );

                            // Collect notification info to send push after commit
                            let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

                            // Create notification for post author
                            if let Some(post_author_id) = post_author_user_id {
                                match create_notification(
                                    &mut tx,
                                    CreateNotificationParams {
                                        recipient_id: post_author_id,
                                        actor_id: persisted_actor.id,
                                        notification_type: NotificationType::Reaction,
                                        post_id: Some(post_id),
                                        comment_id: None,
                                        reaction_iri: Some(reaction.iri.clone()),
                                        guestbook_entry_id: None,
                                    },
                                )
                                .await
                                {
                                    Ok(notification) => {
                                        tracing::info!("Created notification for ❤️ reaction from federated actor");
                                        notification_info.push((notification.id, post_author_id));
                                    }
                                    Err(e) => tracing::warn!(
                                        "Failed to create notification for ❤️ reaction: {:?}",
                                        e
                                    ),
                                }
                            }

                            tx.commit().await?;

                            // Send push notifications
                            if !notification_info.is_empty() {
                                let push_service = data.push_service.clone();
                                let db_pool = data.db_pool.clone();
                                tokio::spawn(async move {
                                    for (notification_id, recipient_id) in notification_info {
                                        let mut tx = match db_pool.begin().await {
                                            Ok(tx) => tx,
                                            Err(e) => {
                                                tracing::warn!("Failed to begin transaction for push notification: {:?}", e);
                                                continue;
                                            }
                                        };

                                        if let Ok(Some(notification)) = get_notification_by_id(
                                            &mut tx,
                                            notification_id,
                                            recipient_id,
                                        )
                                        .await
                                        {
                                            // The number on the bell, for the icon's badge
                                            let badge_count =
                                                get_badge_count(&mut tx, recipient_id)
                                                    .await
                                                    .ok()
                                                    .and_then(|count| u32::try_from(count).ok());

                                            send_push_for_notification(
                                                &push_service,
                                                &db_pool,
                                                &notification,
                                                badge_count,
                                            )
                                            .await;
                                        }
                                        let _ = tx.commit().await;
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create reaction from Like: {:?}", e);
                            tx.rollback().await?;
                        }
                    }
                } else {
                    tracing::debug!("Post {} not found for Like activity", post_id);
                }
            } else {
                tracing::warn!("Failed to parse post ID from Like object: {}", post_id_str);
            }
        } else {
            tracing::debug!(
                "Like object URL doesn't match local post pattern: {}",
                object_url
            );
        }

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EmojiReact {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "actor_from_signature_deser"
    )]
    pub actor: Option<ObjectId<Actor>>,
    #[serde(rename = "object")]
    pub object: Url,
    pub content: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: Url,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    pub to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<serde_json::Value>,
}

#[async_trait::async_trait]
impl Activity for EmojiReact {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        // If actor is not provided, use object ID as fallback
        // We'll extract the real actor from signature in the receive method
        if let Some(actor) = &self.actor {
            actor.inner()
        } else {
            // Fallback to object ID for now - we'll handle actor extraction in receive()
            &self.object
        }
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("=== RECEIVED EMOJIREACT ACTIVITY ===");

        // Try to get actor URL from direct field or extract from signature
        let actor_url = if let Some(actor) = &self.actor {
            tracing::info!("Actor: {}", actor.inner());
            Some(actor.inner().clone())
        } else {
            tracing::info!("Actor: None (missing from activity)");
            // Try to extract from signature
            if let Some(signature) = &self.signature {
                if let Some(creator) = signature.get("creator").and_then(|v| v.as_str()) {
                    if let Ok(mut creator_url) = creator.parse::<Url>() {
                        creator_url.set_fragment(None); // Remove #main-key fragment
                        tracing::info!("Extracted actor from signature: {}", creator_url);
                        Some(creator_url)
                    } else {
                        tracing::warn!("Failed to parse creator URL from signature: {}", creator);
                        None
                    }
                } else {
                    tracing::warn!("No creator field found in signature");
                    None
                }
            } else {
                tracing::warn!("No signature field found in EmojiReact activity");
                None
            }
        };

        if actor_url.is_none() {
            tracing::error!("Cannot process EmojiReact without actor");
            return Ok(());
        }
        let actor_url = actor_url.ok_or_else(|| anyhow::anyhow!("Missing actor URL"))?;

        tracing::info!("Object: {}", self.object);
        tracing::info!("Emoji: {}", self.content);
        tracing::info!("================================");

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Dereference the actor using the URL we extracted
        let actor_obj_id = ObjectId::<Actor>::parse(actor_url.as_ref())?;
        let actor = actor_obj_id.dereference(data).await?;
        let persisted_actor = Actor::create_or_update_actor(&mut tx, &actor).await?;

        // Parse post IRI to extract post_id
        let object_url = self.object.to_string();

        // Try to extract post ID from either URL format:
        // https://domain/@username/post-id or https://domain/ap/posts/post-id
        let user_post_prefix = format!("https://{}/@", data.app_data().config.domain);
        let ap_post_prefix = format!("https://{}/ap/posts/", data.app_data().config.domain);

        let post_id_str = if object_url.starts_with(&user_post_prefix) {
            // Extract from URLs like https://domain/@username/post-id
            let path_part = &object_url[user_post_prefix.len()..];
            path_part.find('/').map(|pos| &path_part[pos + 1..])
        } else if object_url.starts_with(&ap_post_prefix) {
            // Extract from URLs like https://domain/ap/posts/post-id
            Some(&object_url[ap_post_prefix.len()..])
        } else {
            None
        };

        if let Some(post_id_str) = post_id_str {
            if let Ok(post_id) = Uuid::parse_str(post_id_str) {
                // Verify post exists and get post author
                if let Some(post) = find_post_by_id(&mut tx, post_id).await? {
                    // Get post author's user_id
                    let post_author_user_id = post
                        .get("author_id")
                        .and_then(|id| id.as_ref())
                        .and_then(|id_str| Uuid::parse_str(id_str).ok());

                    // Create reaction using EmojiReact's IRI and emoji content
                    use crate::models::reaction::create_reaction_from_activitypub;
                    match create_reaction_from_activitypub(
                        &mut tx,
                        self.id.to_string(),
                        post_id,
                        persisted_actor.id,
                        self.content.clone(),
                    )
                    .await
                    {
                        Ok(reaction) => {
                            tracing::info!(
                                "Created {} reaction from EmojiReact activity for post {}",
                                self.content,
                                post_id
                            );

                            // Collect notification info to send push after commit
                            let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

                            // Create notification for post author
                            if let Some(post_author_id) = post_author_user_id {
                                match create_notification(
                                    &mut tx,
                                    CreateNotificationParams {
                                        recipient_id: post_author_id,
                                        actor_id: persisted_actor.id,
                                        notification_type: NotificationType::Reaction,
                                        post_id: Some(post_id),
                                        comment_id: None,
                                        reaction_iri: Some(reaction.iri.clone()),
                                        guestbook_entry_id: None,
                                    },
                                )
                                .await
                                {
                                    Ok(notification) => {
                                        tracing::info!("Created notification for {} reaction from federated actor", self.content);
                                        notification_info.push((notification.id, post_author_id));
                                    }
                                    Err(e) => tracing::warn!(
                                        "Failed to create notification for {} reaction: {:?}",
                                        self.content,
                                        e
                                    ),
                                }
                            }

                            tx.commit().await?;

                            // Send push notifications
                            if !notification_info.is_empty() {
                                let push_service = data.push_service.clone();
                                let db_pool = data.db_pool.clone();
                                tokio::spawn(async move {
                                    for (notification_id, recipient_id) in notification_info {
                                        let mut tx = match db_pool.begin().await {
                                            Ok(tx) => tx,
                                            Err(e) => {
                                                tracing::warn!("Failed to begin transaction for push notification: {:?}", e);
                                                continue;
                                            }
                                        };

                                        if let Ok(Some(notification)) = get_notification_by_id(
                                            &mut tx,
                                            notification_id,
                                            recipient_id,
                                        )
                                        .await
                                        {
                                            // The number on the bell, for the icon's badge
                                            let badge_count =
                                                get_badge_count(&mut tx, recipient_id)
                                                    .await
                                                    .ok()
                                                    .and_then(|count| u32::try_from(count).ok());

                                            send_push_for_notification(
                                                &push_service,
                                                &db_pool,
                                                &notification,
                                                badge_count,
                                            )
                                            .await;
                                        }
                                        let _ = tx.commit().await;
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to create reaction from EmojiReact: {:?}", e);
                            tx.rollback().await?;
                        }
                    }
                } else {
                    tracing::debug!("Post {} not found for EmojiReact activity", post_id);
                }
            } else {
                tracing::warn!(
                    "Failed to parse post ID from EmojiReact object: {}",
                    post_id_str
                );
            }
        } else {
            tracing::debug!(
                "EmojiReact object URL doesn't match local post pattern: {}",
                object_url
            );
        }

        Ok(())
    }
}
