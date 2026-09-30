//! Announcing, updating and deleting what is already out there.

use activitystreams_kinds::activity::{AnnounceType, DeleteType, UpdateType};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::app_error::AppError;
use crate::models::actor::Actor;

use super::{actor_from_signature_deser, generate_object_id, string_or_vec_deser, ActorObject};

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Announce {
    actor: Url,
    object: Url,
    r#type: AnnounceType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl Announce {
    pub fn new(
        actor: Url,
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

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    actor: Url,
    object: ActorObject,
    r#type: UpdateType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl Update {
    pub fn new(
        actor: Url,
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

pub async fn send_update_activity(
    actor: &Actor,
    app_state: &crate::web::state::AppState,
) -> Result<(), AppError> {
    use crate::models::follow::get_follower_shared_inboxes_for_actor;

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

    // Create the updated actor object
    let actor_object = super::actor_object(actor.clone(), &app_state.uris)?;

    // Generate activity ID
    let activity_id = generate_object_id(&app_state.config.domain)?;

    // Set up audience - public update
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];

    // Create Update activity
    let update_activity = Update::new(
        actor.iri.url().clone(),
        actor_object,
        activity_id,
        to,
        cc,
        chrono::Utc::now().to_rfc3339(),
    );

    // Send the activity to followers
    actor.send(update_activity, inbox_urls, app_state).await?;

    Ok(())
}

pub async fn send_delete_activity(
    actor: &Actor,
    object_url: Url,
    app_state: &crate::web::state::AppState,
) -> Result<(), AppError> {
    use crate::models::follow::get_follower_shared_inboxes_for_actor;

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
        actor.iri.url().clone(),
        tombstone,
        activity_id,
        to,
        cc,
        chrono::Utc::now().to_rfc3339(),
    );

    // Send the activity to followers
    actor.send(delete_activity, inbox_urls, app_state).await?;

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
    actor: Option<Url>,
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
        actor: Url,
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
