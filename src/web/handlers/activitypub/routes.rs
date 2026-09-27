//! What other servers fetch and post: webfinger, actors, posts and the inboxes.

use activitypub_federation::axum::inbox::{receive_activity, ActivityData};
use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::traits::Activity;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::post::find_post_by_id;
use crate::web::state::AppState;

use super::{Create, Delete, EmojiReact, Follow, Like, Undo, Update};

// Serving to other servers is feder's (src/federation/serving.rs). What
// reaches these is a person following an ActivityPub ID in a browser, who
// is sent to the page it is the ID of.

/// `/ap/users/{user_id}`, asked for as a page: the profile.
pub async fn activitypub_user_page(
    Path(user_id): Path<String>,
    data: Data<AppState>,
) -> Result<Response, AppError> {
    let Ok(user_id) = Uuid::parse_str(&user_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = data.app_data().db_pool.begin().await?;
    Ok(match Actor::find_by_user_id(&mut tx, user_id).await? {
        Some(actor) => Redirect::to(&actor.url).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

/// `/ap/communities/{community_id}`, asked for as a page: the community.
pub async fn activitypub_community_page(
    Path(community_id): Path<String>,
    data: Data<AppState>,
) -> Result<Response, AppError> {
    let Ok(community_id) = Uuid::parse_str(&community_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = data.app_data().db_pool.begin().await?;
    Ok(
        match Actor::find_by_community_id(&mut tx, community_id).await? {
            Some(actor) => Redirect::to(&actor.url).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
    )
}

/// `/ap/posts/{post_id}`, asked for as a page: the post.
pub async fn activitypub_post_page(
    Path(post_id): Path<String>,
    data: Data<AppState>,
) -> Result<Response, AppError> {
    let Ok(post_id) = Uuid::parse_str(&post_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut tx = data.app_data().db_pool.begin().await?;
    let Some(post) = find_post_by_id(&mut tx, post_id).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let Some(login_name) = post.get("login_name").and_then(|v| v.as_deref()) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let community_slug = post.get("community_slug").and_then(|v| v.as_deref());
    let url = crate::models::post::post_page_url(
        &data.app_data().config.domain,
        login_name,
        community_slug,
        post_id,
    );
    Ok(Redirect::to(&url).into_response())
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
