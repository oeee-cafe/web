//! Announcing, updating and deleting what is already out there.

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::{Activity, Object};

use activitystreams_kinds::activity::{AnnounceType, DeleteType, UpdateType};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::app_error::AppError;
use crate::live::LiveEvent;
use crate::models::actor::Actor;
use crate::models::comment::{delete_comment_by_iri, find_comment_by_iri};
use crate::models::notification::BadgeFalls;
use crate::web::state::AppState;

use super::{actor_from_signature_deser, generate_object_id, string_or_vec_deser, ActorObject};

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Announce {
    actor: ObjectId<Actor>,
    object: Url,
    r#type: AnnounceType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl Announce {
    pub fn new(
        actor: ObjectId<Actor>,
        object: Url,
        id: Url,
        to: Vec<String>,
        cc: Vec<String>,
        published: String,
    ) -> Announce {
        Announce {
            actor,
            object,
            r#type: Default::default(),
            id,
            to,
            cc,
            published,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Announce {
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

    async fn receive(self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        // Announce activities are typically sent outbound, not received
        // If we wanted to handle incoming Announce activities (e.g., boosts from other servers),
        // we would implement the logic here
        tracing::info!("Received Announce activity: {:?}", self);
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    actor: ObjectId<Actor>,
    object: ActorObject,
    r#type: UpdateType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl Update {
    pub fn new(
        actor: ObjectId<Actor>,
        object: ActorObject,
        id: Url,
        to: Vec<String>,
        cc: Vec<String>,
        published: String,
    ) -> Update {
        Update {
            actor,
            object,
            r#type: Default::default(),
            id,
            to,
            cc,
            published,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Update {
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
        // Update activities notify followers about actor profile changes
        // We would typically update our local copy of the actor here
        tracing::info!("Received Update activity: {:?}", self);

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Find the actor being updated
        let actor = Actor::find_by_iri(&mut tx, self.actor.to_string()).await?;
        if let Some(mut actor) = actor {
            // Update actor fields based on the object
            match &self.object {
                ActorObject::Person(person) => {
                    actor.name = person.name.clone();
                    actor.username = person.preferred_username.clone();
                    actor.url = person.url.to_string();
                }
                ActorObject::Group(group) => {
                    actor.name = group.name.clone();
                    actor.username = group.preferred_username.clone();
                    actor.url = group.url.to_string();
                }
            }

            // Update the actor in the database
            Actor::create_or_update_actor(&mut tx, &actor).await?;
            tx.commit().await?;
        }

        Ok(())
    }
}

pub async fn send_update_activity(
    actor: &Actor,
    app_state: &crate::web::state::AppState,
) -> Result<(), AppError> {
    use crate::models::follow::get_follower_shared_inboxes_for_actor;
    use activitypub_federation::config::FederationConfig;

    let db = &app_state.db_pool;
    let mut tx = db.begin().await?;

    // Get follower inboxes
    let follower_inboxes = get_follower_shared_inboxes_for_actor(&mut tx, actor.id).await?;
    tx.commit().await?;

    if follower_inboxes.is_empty() {
        return Ok(());
    }

    // Convert inboxes to Urls
    let inbox_urls: Result<Vec<Url>, _> = follower_inboxes
        .into_iter()
        .map(|inbox| inbox.parse::<Url>())
        .collect();
    let inbox_urls = inbox_urls?;

    // Create federation config and data
    let federation_config = FederationConfig::builder()
        .domain(app_state.config.domain.clone())
        .app_data(app_state.clone())
        .build()
        .await?;
    let federation_data = federation_config.to_request_data();

    // Create the updated actor object
    let actor_object = actor.clone().into_json(&federation_data).await?;

    // Generate activity ID
    let activity_id = generate_object_id(&app_state.config.domain)?;

    // Set up audience - public update
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];

    // Create Update activity
    let update_activity = Update::new(
        ObjectId::parse(&actor.iri)?,
        actor_object,
        activity_id,
        to,
        cc,
        chrono::Utc::now().to_rfc3339(),
    );

    // Send the activity to followers
    actor
        .send(
            update_activity,
            inbox_urls,
            app_state.config.use_activitypub_queue(),
            &federation_data,
        )
        .await?;

    Ok(())
}

pub async fn send_delete_activity(
    actor: &Actor,
    object_url: Url,
    app_state: &crate::web::state::AppState,
) -> Result<(), AppError> {
    use crate::models::follow::get_follower_shared_inboxes_for_actor;
    use activitypub_federation::config::FederationConfig;

    let db = &app_state.db_pool;
    let mut tx = db.begin().await?;

    // Get follower inboxes
    let follower_inboxes = get_follower_shared_inboxes_for_actor(&mut tx, actor.id).await?;
    tx.commit().await?;

    if follower_inboxes.is_empty() {
        return Ok(());
    }

    // Convert inboxes to Urls
    let inbox_urls: Result<Vec<Url>, _> = follower_inboxes
        .into_iter()
        .map(|inbox| inbox.parse::<Url>())
        .collect();
    let inbox_urls = inbox_urls?;

    // Create federation config and data
    let federation_config = FederationConfig::builder()
        .domain(app_state.config.domain.clone())
        .app_data(app_state.clone())
        .build()
        .await?;
    let federation_data = federation_config.to_request_data();

    // Generate activity ID
    let activity_id = generate_object_id(&app_state.config.domain)?;

    // Set up audience - public delete
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];

    // Create Tombstone object
    let tombstone = Tombstone {
        id: object_url,
        r#type: "Tombstone".to_string(),
    };

    // Create Delete activity
    let delete_activity = Delete::new(
        ObjectId::parse(&actor.iri)?,
        tombstone,
        activity_id,
        to,
        cc,
        chrono::Utc::now().to_rfc3339(),
    );

    // Send the activity to followers
    actor
        .send(
            delete_activity,
            inbox_urls,
            app_state.config.use_activitypub_queue(),
            &federation_data,
        )
        .await?;

    Ok(())
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    id: Url,
    r#type: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Delete {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "actor_from_signature_deser"
    )]
    actor: Option<ObjectId<Actor>>,
    object: Tombstone,
    r#type: DeleteType,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Url>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
    // Add signature field to extract actor from
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<serde_json::Value>,
}

impl Delete {
    pub fn new(
        actor: ObjectId<Actor>,
        object: Tombstone,
        id: Url,
        to: Vec<String>,
        cc: Vec<String>,
        published: String,
    ) -> Delete {
        Delete {
            actor: Some(actor),
            object,
            r#type: Default::default(),
            id: Some(id),
            to,
            cc,
            published: Some(published),
            signature: None,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Delete {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        // For Delete activities without explicit ID, we could generate one or use the object ID
        // For now, return a placeholder URL that will need to be handled properly
        self.id.as_ref().unwrap_or({
            // This is a fallback - we'll need to handle this case properly
            &self.object.id
        })
    }

    fn actor(&self) -> &Url {
        // If actor is not provided, use object ID as fallback
        // We'll extract the real actor from signature in the receive method
        if let Some(actor) = &self.actor {
            actor.inner()
        } else {
            // Fallback to object ID for now - we'll handle actor extraction in receive()
            &self.object.id
        }
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("=== RECEIVED DELETE ACTIVITY ===");

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
                        creator_url.set_fragment(None); // Remove #key-2 fragment
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
                tracing::warn!("No signature field found in Delete activity");
                None
            }
        };

        tracing::info!("Object: {}", self.object.id);
        if let Some(id) = &self.id {
            tracing::info!("Activity ID: {}", id);
        } else {
            tracing::info!("Activity ID: None (missing from activity)");
        }
        tracing::info!("================================");

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        let object_url = self.object.id.to_string();

        // Check if this is a post deletion by trying to parse the object URL
        if let Some(post_id_str) = object_url.strip_prefix(&format!(
            "https://{}/ap/posts/",
            data.app_data().config.domain
        )) {
            if let Ok(post_id) = uuid::Uuid::parse_str(post_id_str) {
                // Mark the post as deleted in our database
                use crate::models::post::{delete_post, PostDeletionReason};
                let falls =
                    match delete_post(&mut tx, post_id, PostDeletionReason::UserDeleted).await {
                        Ok(falls) => falls,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to delete post {} from Delete activity: {:?}",
                                post_id,
                                e
                            );
                            BadgeFalls::none()
                        }
                    };
                tx.commit().await?;
                data.push_service.badges_fell(falls);
            }
        } else {
            // Check if this is a comment deletion by IRI
            // Try to find a comment with this IRI
            if let Some(comment) = find_comment_by_iri(&mut tx, &object_url).await? {
                // Verify that the actor attempting deletion owns the comment
                let deleting_actor = if let Some(actor_url) = &actor_url {
                    Actor::read_from_id(actor_url.clone(), data).await?
                } else {
                    tracing::warn!("Delete activity missing actor field and could not extract from signature - cannot verify ownership");
                    None
                };

                if let Some(deleting_actor) = deleting_actor {
                    if comment.actor_id == deleting_actor.id {
                        // Actor owns the comment, proceed with deletion
                        if let Some(falls) = delete_comment_by_iri(&mut tx, &object_url).await? {
                            tracing::info!("Deleted comment with IRI: {}", object_url);
                            tx.commit().await?;
                            data.push_service.badges_fell(falls);
                            data.live.publish(LiveEvent::Comments {
                                post_id: comment.post_id,
                                by: None,
                            });
                        } else {
                            tracing::warn!("Failed to delete comment with IRI: {}", object_url);
                        }
                    } else {
                        tracing::warn!(
                            "Actor {} attempted to delete comment {} owned by different actor {}",
                            deleting_actor.id,
                            object_url,
                            comment.actor_id
                        );
                    }
                } else if let Some(actor_url) = &actor_url {
                    tracing::warn!(
                        "Could not find deleting actor for Delete activity: {}",
                        actor_url
                    );
                } else {
                    tracing::warn!(
                        "Delete activity missing actor field and could not extract from signature"
                    );
                }
            } else {
                tracing::debug!(
                    "No local object found for Delete activity IRI: {}",
                    object_url
                );
            }
        }

        Ok(())
    }
}
