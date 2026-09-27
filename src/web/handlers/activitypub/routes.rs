//! What other servers fetch and post: webfinger, actors, posts and the inboxes.

use activitypub_federation::axum::inbox::{receive_activity, ActivityData};
use activitypub_federation::axum::json::FederationJson;
use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::fetch::webfinger::{build_webfinger_response, extract_webfinger_name};
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::traits::{Activity, Object};

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::{create_actor_for_user, Actor};
use crate::models::community::{find_community_by_id, find_community_by_slug, CommunityVisibility};
use crate::models::post::find_post_by_id;
use crate::models::user::{find_user_by_id, find_user_by_login_name};
use crate::web::state::AppState;

use super::{create_note_from_post, Create, Delete, EmojiReact, Follow, Like, Undo, Update};

#[derive(Deserialize)]
pub struct WebfingerQuery {
    resource: String,
}

pub async fn activitypub_webfinger(
    Query(query): Query<WebfingerQuery>,
    data: Data<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let name = extract_webfinger_name(&query.resource, &data)?;
    let db = &data.app_data().db_pool;
    let mut tx = db.begin().await?;

    // First, try to find a user with this login name
    let user = find_user_by_login_name(&mut tx, name).await?;
    if let Some(user) = user {
        let actor = Actor::find_by_user_id(&mut tx, user.id).await?;
        if let Some(actor) = actor {
            return Ok(Json(build_webfinger_response(
                query.resource,
                actor
                    .iri
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid actor IRI: {}", e))?,
            ))
            .into_response());
        }
    }

    // If no user found, try to find a community with this slug
    let community = find_community_by_slug(&mut tx, name.to_string()).await?;
    if let Some(community) = community {
        // Only allow webfinger discovery for public and unlisted communities
        // Private communities should not be discoverable via webfinger
        if community.visibility == CommunityVisibility::Private {
            return Ok((StatusCode::NOT_FOUND, "Community not found").into_response());
        }

        let actor = Actor::find_by_community_id(&mut tx, community.id).await?;
        if let Some(actor) = actor {
            return Ok(Json(build_webfinger_response(
                query.resource,
                actor
                    .iri
                    .parse()
                    .map_err(|e| anyhow::anyhow!("Invalid actor IRI: {}", e))?,
            ))
            .into_response());
        }
    }

    // Neither user nor community found
    Ok((StatusCode::NOT_FOUND, "User or community not found").into_response())
}

pub async fn activitypub_get_user(
    _header_map: HeaderMap,
    Path(user_id): Path<String>,
    data: Data<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let db = &data.app_data().db_pool;
    let mut tx = db.begin().await?;

    if let Some(actor) = Actor::find_by_user_id(
        &mut tx,
        Uuid::parse_str(&user_id)
            .map_err(|e| anyhow::anyhow!("Invalid user UUID: {}: {}", user_id, e))?,
    )
    .await?
    {
        let json_actor = actor.into_json(&data).await?;
        let context = [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ];

        let activity = WithContext::new(
            json_actor,
            serde_json::Value::Array(
                context
                    .into_iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            ),
        );
        Ok(FederationJson(activity).into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, "Actor not found").into_response())
    }
}

pub async fn activitypub_get_community(
    _header_map: HeaderMap,
    Path(community_id): Path<String>,
    data: Data<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let db = &data.app_data().db_pool;
    let mut tx = db.begin().await?;

    if let Some(actor) = Actor::find_by_community_id(
        &mut tx,
        Uuid::parse_str(&community_id)
            .map_err(|e| anyhow::anyhow!("Invalid community UUID: {}: {}", community_id, e))?,
    )
    .await?
    {
        let json_actor = actor.into_json(&data).await?;
        let context = [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ];

        let activity = WithContext::new(
            json_actor,
            serde_json::Value::Array(
                context
                    .into_iter()
                    .map(|s| serde_json::Value::String(s.to_string()))
                    .collect(),
            ),
        );
        Ok(FederationJson(activity).into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, "Actor not found").into_response())
    }
}

pub async fn activitypub_get_post(
    _header_map: HeaderMap,
    Path(post_id): Path<String>,
    data: Data<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let db = &data.app_data().db_pool;
    let mut tx = db.begin().await?;

    let post_uuid = Uuid::parse_str(&post_id)?;

    if let Some(post) = find_post_by_id(&mut tx, post_uuid).await? {
        // Check community visibility - only expose posts from public and unlisted communities via ActivityPub
        // Private community posts should not be accessible
        // Personal posts (no community) are always accessible
        let community_id = post
            .get("community_id")
            .and_then(|v| v.as_ref())
            .and_then(|s| Uuid::parse_str(s).ok());

        if let Some(cid) = community_id {
            let community = find_community_by_id(&mut tx, cid).await?;
            if let Some(community) = community
                && community.visibility == CommunityVisibility::Private
            {
                return Ok((StatusCode::NOT_FOUND, "Post not found").into_response());
            }
        }

        let author_id = Uuid::parse_str(
            post.get("author_id")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Missing author_id in post"))?,
        )?;

        // Find the author's actor, create if it doesn't exist
        let author_actor = Actor::find_by_user_id(&mut tx, author_id).await?;
        let author_actor = if let Some(actor) = author_actor {
            actor
        } else {
            // Actor doesn't exist, try to find the user and create the actor
            if let Some(user) = find_user_by_id(&mut tx, author_id).await? {
                tracing::info!(
                    "Creating missing actor for user {} (id: {})",
                    user.login_name,
                    user.id
                );
                create_actor_for_user(&mut tx, &user, &data.app_data().config).await?
            } else {
                return Ok((StatusCode::NOT_FOUND, "User not found").into_response());
            }
        };

        // Use the shared function to create the Note
        let note = create_note_from_post(
            &mut tx,
            post_uuid,
            &author_actor,
            &data.app_data().config.domain,
            &data.app_data().config.r2_public_endpoint_url,
        )
        .await?;

        // Commit the transaction to persist any actor creations
        tx.commit().await?;

        let context = [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ];

        Ok(FederationJson(WithContext::new(
            note,
            Value::Array(
                context
                    .into_iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
        ))
        .into_response())
    } else {
        Ok((StatusCode::NOT_FOUND, "Post not found").into_response())
    }
}

pub async fn activitypub_post_user_inbox(
    data: Data<AppState>,
    activity_data: ActivityData,
) -> impl IntoResponse {
    tracing::warn!("🔔 USER INBOX: Request received at /ap/users/*/inbox");
    tracing::info!("=== USER INBOX RECEIVED ACTIVITY ===");

    // Enhanced debug logging to diagnose Delete activity issues
    tracing::info!("=== DEBUG: About to call receive_activity function ===");
    tracing::info!("=== DEBUG: If you don't see any more logs after this, the issue is in receive_activity itself ===");

    // Debug: Log that we received an activity (we can't access body directly due to privacy)
    tracing::info!("Attempting to process ActivityPub activity");
    tracing::debug!("Available activity types in enum: Create, Follow, Undo, Update, Delete");

    let result = receive_activity::<WithContext<PersonAcceptedActivities>, Actor, AppState>(
        activity_data,
        &data,
    )
    .await;

    tracing::info!("=== DEBUG: receive_activity function completed ===");

    if let Err(ref e) = result {
        tracing::error!("Activity processing failed: {:?}", e);
        let error_str = format!("{:?}", e);
        if error_str.contains("data did not match any variant") {
            tracing::error!("This appears to be an enum variant matching error - the activity JSON structure doesn't match any of our defined variants");
            tracing::error!("This usually means there's a field mismatch in our Create, Follow, Undo, Update, or Delete structs");
            tracing::error!("Available activity types: Create, Follow, Undo, Update, Delete");
            // Check if this might be a Delete activity with problematic fields
            if error_str.contains("Delete") {
                tracing::error!("This appears to be a Delete activity that failed to deserialize");
                tracing::error!("Common issues: URL fragments in ID field, missing fields, or Tombstone object format");
            }
        }
    } else {
        tracing::info!("Activity processed successfully");
    }

    result
}

pub async fn activitypub_post_community_inbox(
    data: Data<AppState>,
    activity_data: ActivityData,
) -> impl IntoResponse {
    tracing::warn!("🔔 COMMUNITY INBOX: Request received at /ap/communities/*/inbox");
    receive_activity::<WithContext<GroupAcceptedActivities>, Actor, AppState>(activity_data, &data)
        .await
}

pub async fn activitypub_post_user_followers(
    Path(user_id): Path<String>,
    data: Data<AppState>,
) -> impl IntoResponse {
    tracing::warn!(
        "🔔 USER FOLLOWERS: Request received at /ap/users/{}/followers",
        user_id
    );

    let domain = &data.app_data().config.domain;
    let followers_url = format!("https://{}/ap/users/{}/followers", domain, user_id);

    // Return empty OrderedCollection following ActivityPub spec
    let collection = serde_json::json!({
        "type": "OrderedCollection",
        "id": followers_url,
        "@context": "https://www.w3.org/ns/activitystreams",
        "totalItems": 0,
    });

    Json(collection)
}

pub async fn activitypub_post_shared_inbox(
    data: Data<AppState>,
    activity_data: ActivityData,
) -> impl IntoResponse {
    tracing::warn!("🔔 SHARED INBOX: Request received at /ap/inbox");
    tracing::info!("=== SHARED INBOX RECEIVED ACTIVITY ===");
    tracing::info!("=== DEBUG: About to call receive_activity function for shared inbox ===");
    tracing::info!("=== DEBUG: If you don't see any more logs after this, the issue is in receive_activity itself ===");

    // Use the same PersonAcceptedActivities as user inbox for now
    let result = receive_activity::<WithContext<PersonAcceptedActivities>, Actor, AppState>(
        activity_data,
        &data,
    )
    .await;

    tracing::info!("=== DEBUG: shared inbox receive_activity function completed ===");

    if let Err(ref e) = result {
        tracing::error!("Shared inbox activity processing failed: {:?}", e);
        let error_str = format!("{:?}", e);
        if error_str.contains("data did not match any variant") {
            tracing::error!("This appears to be an enum variant matching error in shared inbox - the activity JSON structure doesn't match any of our defined variants");
            tracing::error!("This usually means there's a field mismatch in our Create, Follow, Undo, Update, or Delete structs");
            tracing::error!(
                "Available activity types: Create, Follow, Undo, Update, Delete, Unknown"
            );
            // Check if this might be a Delete activity with problematic fields
            if error_str.contains("Delete") {
                tracing::error!("This appears to be a Delete activity that failed to deserialize in shared inbox");
                tracing::error!("Common issues: URL fragments in ID field, missing fields, or Tombstone object format");
            }
        }
    } else {
        tracing::info!("Shared inbox activity processed successfully");
    }
    result
}

/// List of all activities which this actor can receive.
#[derive(Deserialize, Serialize, Debug)]
#[serde(untagged)]
#[enum_delegate::implement(Activity)]
pub enum PersonAcceptedActivities {
    Create(Create),
    Follow(Follow),
    Undo(Undo),
    Update(Update),
    Delete(Delete),
    Like(Like),
    EmojiReact(EmojiReact),
    Unknown(UnknownActivity),
}

#[derive(Deserialize, Serialize, Debug)]
pub struct UnknownActivity {
    id: Url,
    actor: ObjectId<Actor>,
    #[serde(flatten)]
    data: serde_json::Value,
}

#[async_trait::async_trait]
impl Activity for UnknownActivity {
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
        tracing::info!("=== RECEIVED UNKNOWN ACTIVITY ===");
        tracing::info!(
            "Raw activity JSON: {}",
            serde_json::to_string_pretty(&self.data)
                .unwrap_or_else(|_| "Failed to serialize".to_string())
        );

        // Check if this looks like a Delete activity
        if let Some(activity_type) = self.data.get("type").and_then(|v| v.as_str()) {
            tracing::info!("Unknown activity type: {}", activity_type);

            if activity_type == "Delete" {
                tracing::error!("=== DELETE ACTIVITY REACHED UNKNOWN HANDLER ===");
                tracing::error!("This means the Delete struct is not deserializing properly");
                tracing::error!(
                    "Delete activity JSON: {}",
                    serde_json::to_string_pretty(&self.data)
                        .unwrap_or_else(|_| "Failed to serialize".to_string())
                );

                // Try to manually deserialize this as a Delete to see what fails
                if let Ok(json_str) = serde_json::to_string(&self.data) {
                    match serde_json::from_str::<Delete>(&json_str) {
                        Ok(_) => {
                            tracing::error!(
                                "STRANGE: Delete deserialization worked when tried manually!"
                            );
                        }
                        Err(e) => {
                            tracing::error!("Delete deserialization failed: {:?}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
#[enum_delegate::implement(Activity)]
pub enum GroupAcceptedActivities {
    Follow(Follow),
    Undo(Undo),
    Update(Box<Update>),
    Delete(Box<Delete>),
}
