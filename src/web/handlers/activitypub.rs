use activitypub_federation::axum::inbox::{receive_activity, ActivityData};
use activitypub_federation::axum::json::FederationJson;
use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::fetch::webfinger::{build_webfinger_response, extract_webfinger_name};
use activitypub_federation::kinds::actor::PersonType;
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::protocol::public_key::PublicKey;
use activitypub_federation::protocol::verification::verify_domains_match;
use activitypub_federation::traits::{
    ActivityHandler, Actor as ActivityPubFederationActor, Object,
};

use activitystreams_kinds::activity::{
    AcceptType, AnnounceType, CreateType, DeleteType, FollowType, UndoType, UpdateType,
};
use activitystreams_kinds::actor::GroupType;
use activitystreams_kinds::object::NoteType;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::markdown_utils::process_markdown_content;

fn extract_note_content(note: &Note) -> (String, Option<String>) {
    // Try to get HTML content from contents field or content field
    let raw_html_content = note.content.clone();

    // Sanitize HTML content if present using ammonia defaults
    let html_content = raw_html_content.map(|html| {
        let sanitized = ammonia::clean(&html);

        tracing::debug!(
            "Sanitized HTML content: original length {}, sanitized length {}",
            html.len(),
            sanitized.len()
        );

        if html != sanitized {
            tracing::info!("HTML content was sanitized - potentially dangerous content removed");
        }

        sanitized
    });

    // Try to get markdown content from source field
    let markdown_content = if let Some(source) = &note.source {
        // Parse source as object with content and mediaType
        if let Ok(source_obj) =
            serde_json::from_value::<serde_json::Map<String, Value>>(source.clone())
        {
            if let (Some(Value::String(content)), Some(Value::String(media_type))) =
                (source_obj.get("content"), source_obj.get("mediaType"))
            {
                if media_type == "text/markdown" || media_type == "text/plain" {
                    content.clone()
                } else {
                    // Fallback to sanitized HTML content if available, or "No content"
                    html_content
                        .clone()
                        .unwrap_or_else(|| "No content".to_string())
                }
            } else {
                // Fallback to sanitized HTML content if available, or "No content"
                html_content
                    .clone()
                    .unwrap_or_else(|| "No content".to_string())
            }
        } else {
            // Fallback to sanitized HTML content if available, or "No content"
            html_content
                .clone()
                .unwrap_or_else(|| "No content".to_string())
        }
    } else {
        // No source field, use sanitized HTML content as fallback for markdown too
        html_content
            .clone()
            .unwrap_or_else(|| "No content".to_string())
    };

    (markdown_content, html_content)
}
use crate::models::actor::{create_actor_for_user, Actor, ActorType};
use crate::models::comment::{
    create_comment_from_activitypub, delete_comment_by_iri, find_comment_by_iri,
};
use crate::models::community::{find_community_by_id, find_community_by_slug, CommunityVisibility};
use crate::models::follow;
use crate::models::image::find_image_by_id;
use crate::models::notification::{
    create_notification, get_notification_by_id, get_unread_count, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::models::user::{find_user_by_id, find_user_by_login_name};
use crate::web::state::AppState;

// Custom deserializers for flexible ActivityPub field formats
fn string_or_vec_deser<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(vec![s]),
        Value::Array(arr) => {
            let mut result = Vec::new();
            for item in arr {
                if let Value::String(s) = item {
                    result.push(s);
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

fn actor_from_signature_deser<'de, D>(deserializer: D) -> Result<Option<ObjectId<Actor>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // First try to deserialize as a direct actor field
    match ObjectId::<Actor>::deserialize(deserializer) {
        Ok(actor_id) => Ok(Some(actor_id)),
        Err(_) => {
            // If that fails, return None and we'll try to extract from signature elsewhere
            Ok(None)
        }
    }
}

fn content_or_contents_deser<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(Some(s)),
        Value::Array(arr) => {
            // Take the first string from the array if available
            for item in arr {
                if let Value::String(s) = item {
                    return Ok(Some(s));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn tag_or_vec_deser<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Object(_) => {
            // Single tag object
            let tag: Tag = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(vec![tag])
        }
        Value::Array(arr) => {
            // Array of tag objects
            let mut result = Vec::new();
            for item in arr {
                if let Ok(tag) = serde_json::from_value::<Tag>(item) {
                    result.push(tag);
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    id: ObjectId<Actor>,
    r#type: PersonType,
    preferred_username: String,
    name: String,
    inbox: Url,
    outbox: Url,
    public_key: PublicKey,
    endpoints: serde_json::Value,
    followers: Url,
    manually_approves_followers: bool,
    url: Url,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    id: ObjectId<Actor>,
    r#type: GroupType,
    preferred_username: String,
    name: String,
    inbox: Url,
    outbox: Url,
    public_key: PublicKey,
    endpoints: serde_json::Value,
    followers: Url,
    manually_approves_followers: bool,
    url: Url,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum ActorObject {
    Person(Person),
    Group(Group),
}

#[async_trait::async_trait]
impl Object for Actor {
    type DataType = AppState;
    type Kind = ActorObject;
    type Error = AppError;

    async fn read_from_id(
        object_id: Url,
        data: &Data<Self::DataType>,
    ) -> Result<Option<Self>, Self::Error> {
        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        let actor = Actor::find_by_iri(&mut tx, object_id.to_string()).await?;
        tx.commit().await?;
        Ok(actor)
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        let public_key = PublicKey {
            id: format!("{}#main-key", self.iri)
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
            owner: self
                .iri
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
            public_key_pem: self.public_key_pem,
        };

        let endpoints = serde_json::json!({
            "type": "as:Endpoints",
            "sharedInbox": format!("https://{}/ap/inbox", self.instance_host)
        });

        match self.r#type {
            ActorType::Group => Ok(ActorObject::Group(Group {
                id: ObjectId::parse(&self.iri)?,
                r#type: GroupType::Group,
                inbox: self.inbox_url.parse()?,
                public_key,
                endpoints,
                followers: self.followers_url.parse()?,
                manually_approves_followers: !self.automatically_approves_followers,
                name: self.name,
                outbox: format!("{}/outbox", self.iri).parse()?,
                preferred_username: self.username,
                url: self.url.parse()?,
            })),
            // Handle all other actor types as Person for ActivityPub compatibility
            ActorType::Person
            | ActorType::Service
            | ActorType::Application
            | ActorType::Organization => Ok(ActorObject::Person(Person {
                id: ObjectId::parse(&self.iri)?,
                r#type: PersonType::Person,
                inbox: self.inbox_url.parse()?,
                public_key,
                endpoints,
                followers: self.followers_url.parse()?,
                manually_approves_followers: !self.automatically_approves_followers,
                name: self.name,
                outbox: format!("{}/outbox", self.iri).parse()?,
                preferred_username: self.username,
                url: self.url.parse()?,
            })),
        }
    }

    async fn verify(
        json: &Self::Kind,
        expected_domain: &Url,
        _data: &Data<Self::DataType>,
    ) -> Result<(), Self::Error> {
        let id = match json {
            ActorObject::Person(person) => &person.id,
            ActorObject::Group(group) => &group.id,
        };
        verify_domains_match(id.inner(), expected_domain)?;
        Ok(())
    }

    async fn from_json(
        json: Self::Kind,
        _data: &Data<Self::DataType>,
    ) -> Result<Self, Self::Error> {
        let (
            id,
            inbox,
            public_key,
            endpoints,
            followers,
            manually_approves_followers,
            name,
            preferred_username,
            url,
            actor_type,
        ) = match json {
            ActorObject::Person(person) => (
                person.id,
                person.inbox,
                person.public_key,
                person.endpoints,
                person.followers,
                person.manually_approves_followers,
                person.name,
                person.preferred_username,
                person.url,
                ActorType::Person,
            ),
            ActorObject::Group(group) => (
                group.id,
                group.inbox,
                group.public_key,
                group.endpoints,
                group.followers,
                group.manually_approves_followers,
                group.name,
                group.preferred_username,
                group.url,
                ActorType::Group,
            ),
        };

        // Parse instance host from the actor ID URL
        let actor_url = id.inner();
        let instance_host = actor_url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Could not extract host from actor URL"))?
            .to_string();

        // Create handle components
        let handle_host = instance_host.clone();
        let handle = format!("@{}@{}", preferred_username, handle_host);

        // Get shared inbox URL from endpoints if available, otherwise use main inbox
        let shared_inbox_url = endpoints
            .get("sharedInbox")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| inbox.as_str())
            .to_string();

        Ok(Actor {
            name,
            iri: id.to_string(),
            inbox_url: inbox.to_string(),
            public_key_pem: public_key.public_key_pem,
            private_key_pem: None,
            id: Uuid::new_v4(),
            url: url.to_string(),
            r#type: actor_type,
            username: preferred_username.clone(),
            instance_host,
            handle_host,
            handle,
            user_id: None,
            community_id: None,
            bio_html: String::new(),
            automatically_approves_followers: !manually_approves_followers,
            shared_inbox_url,
            followers_url: followers.to_string(),
            sensitive: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            published_at: chrono::Utc::now(),
        })
    }
}

impl ActivityPubFederationActor for Actor {
    fn id(&self) -> url::Url {
        self.iri.parse().expect("IRI should be a valid URL")
    }

    fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        self.private_key_pem.clone()
    }

    fn inbox(&self) -> Url {
        self.inbox_url.parse().expect("Inbox URL should be valid")
    }
}

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
    Path(actor_id): Path<String>,
    data: Data<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let db = &data.app_data().db_pool;
    let mut tx = db.begin().await?;

    if let Some(actor) = Actor::find_by_user_id(
        &mut tx,
        Uuid::parse_str(&actor_id)
            .map_err(|e| anyhow::anyhow!("Invalid actor UUID: {}: {}", actor_id, e))?,
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
            if let Some(community) = community {
                if community.visibility == CommunityVisibility::Private {
                    return Ok((StatusCode::NOT_FOUND, "Post not found").into_response());
                }
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
    Path(login_name): Path<String>,
    data: Data<AppState>,
) -> impl IntoResponse {
    tracing::warn!(
        "🔔 USER FOLLOWERS: Request received at /ap/users/{}/followers",
        login_name
    );

    let domain = &data.app_data().config.domain;
    let followers_url = format!("https://{}/ap/users/{}/followers", domain, login_name);

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
#[enum_delegate::implement(ActivityHandler)]
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
impl ActivityHandler for UnknownActivity {
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
#[enum_delegate::implement(ActivityHandler)]
pub enum GroupAcceptedActivities {
    Follow(Follow),
    Undo(Undo),
    Update(Box<Update>),
    Delete(Box<Delete>),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    pub(crate) actor: ObjectId<Actor>,
    pub(crate) object: ObjectId<Actor>,
    #[serde(rename = "type")]
    r#type: FollowType,
    id: Url,
}

impl Follow {
    pub fn new(actor: ObjectId<Actor>, object: ObjectId<Actor>, id: Url) -> Follow {
        Follow {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl ActivityHandler for Follow {
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

    // Ignore clippy false positive: https://github.com/rust-lang/rust-clippy/issues/6446
    #[allow(clippy::await_holding_lock)]
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("receive: {:?} {:?}", self.actor, self.object);

        // add to followers
        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Find the target actor being followed
        tracing::info!("self.object: {:?}", self.object);
        let following_actor = Actor::find_by_iri(&mut tx, self.object.to_string()).await?;
        tracing::info!("following_actor: {:?}", following_actor);

        let following_actor =
            following_actor.ok_or_else(|| anyhow::anyhow!("Target actor not found"))?;

        // Dereference and persist the follower actor
        let follower_actor = self.actor.dereference(data).await?;
        tracing::info!("follower_actor: {:?}", follower_actor);

        let persisted_follower = match Actor::create_or_update_actor(&mut tx, &follower_actor).await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to persist follower actor: {:?}", e);
                return Err(e.into());
            }
        };
        tracing::info!("persisted_follower: {:?}", persisted_follower);

        // Create the follow relationship
        let follow_relation =
            follow::create_follow_by_actor_ids(&mut tx, persisted_follower.id, following_actor.id)
                .await?;
        tracing::info!("follow_relation: {:?}", follow_relation);

        // Commit the transaction before sending accept
        tx.commit().await?;

        // Send back an accept activity
        let id = generate_object_id(data.domain())?;
        let following_actor_object_id = ObjectId::parse(&following_actor.iri)?;
        let accept = Box::new(Accept::new(following_actor_object_id, self, id.clone()));
        following_actor
            .send(
                accept,
                vec![follower_actor.shared_inbox_or_inbox()],
                data.app_data().config.use_activitypub_queue(),
                data,
            )
            .await?;

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    actor: ObjectId<Actor>,
    object: Follow,
    r#type: AcceptType,
    id: Url,
}

impl Accept {
    pub fn new(actor: ObjectId<Actor>, object: Follow, id: Url) -> Accept {
        Accept {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl ActivityHandler for Accept {
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
        // Accept activities are typically not processed when received
        // They're sent as responses to Follow activities
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum UndoObject {
    Follow(Box<Follow>),
    Like(Box<Like>),
    EmojiReact(Box<EmojiReact>),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    pub actor: ObjectId<Actor>,
    pub object: UndoObject,
    pub r#type: UndoType,
    pub id: Url,
}

impl Undo {
    pub fn new(actor: ObjectId<Actor>, object: Follow, id: Url) -> Undo {
        Undo {
            actor,
            object: UndoObject::Follow(Box::new(object)),
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl ActivityHandler for Undo {
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
        tracing::info!("=== RECEIVED UNDO ACTIVITY ===");
        tracing::info!("Actor: {}", self.actor.inner());

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        match self.object {
            UndoObject::Follow(follow) => {
                tracing::info!("Undo type: Follow");

                // Find the target actor being unfollowed
                let following_actor =
                    Actor::find_by_iri(&mut tx, follow.object.to_string()).await?;
                let following_actor =
                    following_actor.ok_or_else(|| anyhow::anyhow!("Target actor not found"))?;

                // Find the follower actor
                let follower_actor = Actor::find_by_iri(&mut tx, follow.actor.to_string()).await?;
                let follower_actor =
                    follower_actor.ok_or_else(|| anyhow::anyhow!("Follower actor not found"))?;

                // Remove the follow relationship
                follow::unfollow_by_actor_ids(&mut tx, follower_actor.id, following_actor.id)
                    .await?;
                tracing::info!(
                    "Removed follow relationship: {} -> {}",
                    follower_actor.iri,
                    following_actor.iri
                );

                // Delete follow notification
                if let Some(following_user_id) = following_actor.user_id {
                    match sqlx::query!(
                        r#"
                        DELETE FROM notifications
                        WHERE recipient_id = $1
                          AND actor_id = $2
                          AND notification_type = 'follow'
                        "#,
                        following_user_id,
                        follower_actor.id
                    )
                    .execute(&mut *tx)
                    .await
                    {
                        Ok(_) => tracing::info!("Deleted follow notification"),
                        Err(e) => tracing::warn!("Failed to delete follow notification: {:?}", e),
                    }
                }

                tx.commit().await?;
            }
            UndoObject::Like(like) => {
                tracing::info!("Undo type: Like (removing ❤️ reaction)");
                tracing::info!("Reaction IRI: {}", like.id);

                // Delete reaction by IRI
                use crate::models::reaction::delete_reaction_by_iri;
                if delete_reaction_by_iri(&mut tx, like.id.as_str()).await? {
                    tracing::info!("Deleted ❤️ reaction with IRI: {}", like.id);
                    tx.commit().await?;
                } else {
                    tracing::warn!("Failed to delete reaction with IRI: {}", like.id);
                }
            }
            UndoObject::EmojiReact(react) => {
                tracing::info!(
                    "Undo type: EmojiReact (removing {} reaction)",
                    react.content
                );
                tracing::info!("Reaction IRI: {}", react.id);

                // Delete reaction by IRI
                use crate::models::reaction::delete_reaction_by_iri;
                if delete_reaction_by_iri(&mut tx, react.id.as_str()).await? {
                    tracing::info!("Deleted {} reaction with IRI: {}", react.content, react.id);
                    tx.commit().await?;
                } else {
                    tracing::warn!("Failed to delete reaction with IRI: {}", react.id);
                }
            }
        }

        tracing::info!("================================");
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub r#type: String,
    pub url: String,
    pub media_type: String,
    pub name: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: Url,
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<NoteType>,
    #[serde(
        alias = "attributedTo",
        alias = "attribution",
        skip_serializing_if = "Option::is_none"
    )]
    attributed_to: Option<ObjectId<Actor>>,
    #[serde(
        alias = "contents",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "content_or_contents_deser"
    )]
    content: Option<String>,
    #[serde(alias = "tos", default)]
    to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<Url>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    attachment: Vec<Attachment>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "inReplyTo")]
    in_reply_to: Option<Url>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "replyTarget")]
    reply_target: Option<Url>,
    #[serde(
        alias = "tags",
        skip_serializing_if = "Vec::is_empty",
        default,
        deserialize_with = "tag_or_vec_deser"
    )]
    tag: Vec<Tag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<serde_json::Value>,
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
}

/// A `Hashtag` on a Note.
///
/// Outbound, this is how a drawing's tags reach other instances: without them
/// nothing we federate is findable by tag anywhere but here. Inbound it is
/// parsed and then deliberately dropped — a Note arriving here becomes a
/// comment on a local post, and comments carry no tags — but it has to parse or
/// the whole Note does not.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    r#type: String,
    href: Option<Url>,
    name: Option<String>,
}

impl Tag {
    fn hashtag(domain: &str, hashtag: &crate::models::hashtag::PostHashtag) -> Option<Tag> {
        Some(Tag {
            r#type: "Hashtag".to_string(),
            href: format!(
                "https://{}/hashtags/{}",
                domain,
                urlencoding::encode(&hashtag.name)
            )
            .parse()
            .ok(),
            // With the `#`, which is what every implementation expects to read
            // and what the name in the database deliberately does not carry.
            name: Some(format!("#{}", hashtag.display_name)),
        })
    }
}

/// A post's tags as `Hashtag` objects. A tag that cannot be turned into a URL
/// is left out rather than failing the delivery of the drawing.
async fn hashtag_tags(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    post_id: Uuid,
    domain: &str,
) -> Result<Vec<Tag>, AppError> {
    Ok(crate::models::hashtag::get_hashtags_for_post(tx, post_id)
        .await?
        .iter()
        .filter_map(|hashtag| Tag::hashtag(domain, hashtag))
        .collect())
}

pub struct NoteParams {
    pub id: Url,
    pub attributed_to: ObjectId<Actor>,
    pub content: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub published: String,
    pub updated: Option<String>,
    pub url: Url,
    pub attachment: Vec<Attachment>,
    pub tag: Vec<Tag>,
}

impl Note {
    pub fn from_params(params: NoteParams) -> Note {
        Note {
            id: params.id,
            r#type: Some(Default::default()),
            attributed_to: Some(params.attributed_to),
            content: Some(params.content),
            to: params.to,
            cc: params.cc,
            published: Some(params.published),
            updated: params.updated,
            url: Some(params.url),
            attachment: params.attachment,
            in_reply_to: None,
            reply_target: None,
            tag: params.tag,
            source: None,
            extra: std::collections::HashMap::new(),
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    actor: ObjectId<Actor>,
    object: Note,
    r#type: CreateType,
    id: Url,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
}

impl Create {
    pub fn new(
        actor: ObjectId<Actor>,
        object: Note,
        id: Url,
        to: Vec<String>,
        cc: Vec<String>,
        published: String,
    ) -> Create {
        Create {
            actor,
            object,
            r#type: Default::default(),
            id,
            to,
            cc,
            published: Some(published),
            extra: std::collections::HashMap::new(),
        }
    }
}

#[async_trait::async_trait]
impl ActivityHandler for Create {
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
        tracing::info!("=== RECEIVED CREATE ACTIVITY ===");
        tracing::info!("Actor: {}", self.actor.inner());
        tracing::info!("Object ID: {}", self.object.id);
        tracing::info!(
            "Object content preview: {}",
            self.object
                .content
                .as_ref()
                .map(|c| {
                    if c.len() > 100 {
                        format!("{}...", &c[..100])
                    } else {
                        c.clone()
                    }
                })
                .unwrap_or_else(|| "No content".to_string())
        );
        tracing::info!("in_reply_to: {:?}", self.object.in_reply_to);
        tracing::info!("reply_target: {:?}", self.object.reply_target);
        tracing::info!("================================");

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Check if this is a reply to a local post
        // Support both in_reply_to and reply_target (different ActivityPub implementations use different names)
        let reply_target_url = self
            .object
            .in_reply_to
            .as_ref()
            .or(self.object.reply_target.as_ref());

        if let Some(reply_url) = reply_target_url {
            let reply_url_str = reply_url.to_string();

            // Check if this is replying to a local post URL pattern
            // Support both user post URLs (https://domain/@username/post-id) and AP post URLs (https://domain/ap/posts/post-id)
            let user_post_prefix = format!("https://{}/@", data.app_data().config.domain);
            let ap_post_prefix = format!("https://{}/ap/posts/", data.app_data().config.domain);

            let post_id = if reply_url_str.starts_with(&user_post_prefix) {
                // Extract from URLs like https://domain/@username/post-id
                let path_part = &reply_url_str[user_post_prefix.len()..];
                path_part
                    .find('/')
                    .map(|slash_pos| &path_part[slash_pos + 1..])
            } else if reply_url_str.starts_with(&ap_post_prefix) {
                // Extract from URLs like https://domain/ap/posts/post-id
                Some(&reply_url_str[ap_post_prefix.len()..])
            } else {
                None
            };

            if let Some(post_id_str) = post_id {
                if let Ok(post_id) = Uuid::parse_str(post_id_str) {
                    // Verify the post exists and get post author
                    if let Some(post) = find_post_by_id(&mut tx, post_id).await? {
                        // Get post author's user_id
                        let post_author_user_id = post
                            .get("author_id")
                            .and_then(|id| id.as_ref())
                            .and_then(|id_str| Uuid::parse_str(id_str).ok());

                        // Get the actor who sent this comment, fetching from remote if needed
                        let actor = Actor::read_from_id(self.actor.inner().clone(), data).await?;

                        let actor = if let Some(actor) = actor {
                            actor
                        } else {
                            // Actor not found locally, fetch from remote and persist
                            tracing::info!(
                                "Actor not found locally, fetching from remote: {}",
                                self.actor.inner()
                            );

                            match self.actor.dereference(data).await {
                                Ok(remote_actor) => {
                                    tracing::info!(
                                        "Successfully fetched remote actor: {}",
                                        self.actor.inner()
                                    );

                                    // Persist the remote actor
                                    let persisted_actor =
                                        Actor::create_or_update_actor(&mut tx, &remote_actor)
                                            .await?;
                                    tracing::info!(
                                        "Persisted new actor: {} ({})",
                                        persisted_actor.handle,
                                        persisted_actor.iri
                                    );
                                    persisted_actor
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to fetch remote actor {}: {:?}",
                                        self.actor.inner(),
                                        e
                                    );
                                    tx.rollback().await?;
                                    return Ok(());
                                }
                            }
                        };

                        // Create the comment from the ActivityPub note
                        // Extract both markdown and HTML content from the ActivityPub note
                        let (markdown_content, html_content) = extract_note_content(&self.object);
                        let comment = create_comment_from_activitypub(
                            &mut tx,
                            post_id,
                            actor.id,
                            markdown_content,
                            html_content,
                            self.object.id.to_string(),
                        )
                        .await;

                        match comment {
                            Ok(comment) => {
                                tracing::info!(
                                    "Created comment from ActivityPub mention for post {}",
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
                                            actor_id: actor.id,
                                            notification_type: NotificationType::Comment,
                                            post_id: Some(post_id),
                                            comment_id: Some(comment.id),
                                            reaction_iri: None,
                                            guestbook_entry_id: None,
                                        },
                                    )
                                    .await
                                    {
                                        Ok(notification) => {
                                            tracing::info!("Created notification for comment from federated actor");
                                            notification_info
                                                .push((notification.id, post_author_id));
                                        }
                                        Err(e) => tracing::warn!(
                                            "Failed to create notification for comment: {:?}",
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
                                                // Get unread count for badge
                                                let badge_count =
                                                    get_unread_count(&mut tx, recipient_id)
                                                        .await
                                                        .ok()
                                                        .and_then(|count| {
                                                            u32::try_from(count).ok()
                                                        });

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
                                tracing::error!(
                                    "Failed to create comment from ActivityPub mention: {:?}",
                                    e
                                );
                                // Don't return error, just log it
                            }
                        }
                    } else {
                        tracing::debug!("Post {} not found for ActivityPub mention", post_id);
                    }
                }
            }
        }

        Ok(())
    }
}

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
impl ActivityHandler for Announce {
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
impl ActivityHandler for Update {
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

pub fn generate_object_id(domain: &str) -> Result<Url, AppError> {
    Ok(Url::parse(&format!(
        "https://{}/objects/{}",
        domain,
        Uuid::new_v4()
    ))?)
}

pub async fn create_note_from_post(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    post_id: Uuid,
    author_actor: &Actor,
    domain: &str,
    r2_public_endpoint_url: &str,
) -> Result<Note, AppError> {
    // Get post details
    let post = find_post_by_id(tx, post_id).await?;
    let post = post.ok_or_else(|| anyhow::anyhow!("Post not found"))?;

    // Get title and content
    let title = post.get("title").and_then(|t| t.as_ref()).map_or("", |v| v);
    let content = post
        .get("content")
        .and_then(|c| c.as_ref())
        .map_or("", |v| v);

    // Format content with title if available and process as markdown
    let formatted_content = if title.is_empty() {
        process_markdown_content(content)
    } else {
        let combined_content = format!("{}\n\n{}", title, content);
        process_markdown_content(&combined_content)
    };

    // Get attachments if image exists
    let mut attachments = Vec::new();
    if let Some(Some(image_id_str)) = post.get("image_id") {
        if let Ok(image_id) = Uuid::parse_str(image_id_str) {
            if let Ok(image) = find_image_by_id(tx, image_id).await {
                let image_url = format!(
                    "{}/image/{}{}/{}",
                    r2_public_endpoint_url,
                    image.image_filename.chars().next().unwrap_or('0'),
                    image.image_filename.chars().nth(1).unwrap_or('0'),
                    image.image_filename
                );

                let attachment = Attachment {
                    r#type: "Image".to_string(),
                    url: image_url,
                    media_type: "image/png".to_string(),
                    name: Some(title.to_string()),
                    width: Some(image.width),
                    height: Some(image.height),
                };
                attachments.push(attachment);
            }
        }
    }

    // Create URLs and IDs
    let post_url: Url =
        crate::models::post::post_page_url(domain, &author_actor.username, post_id).parse()?;

    let note_id: Url = format!("https://{}/ap/posts/{}", domain, post_id).parse()?;

    // Set up audience - public post
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", author_actor.iri)];

    // Get published date
    let published = post
        .get("published_at_utc")
        .ok_or_else(|| anyhow::anyhow!("Missing published_at_utc"))?;

    let note = Note::from_params(NoteParams {
        id: note_id,
        attributed_to: ObjectId::<Actor>::parse(&author_actor.iri)?,
        content: formatted_content,
        to,
        cc,
        published: published.clone().unwrap_or_default(),
        updated: None,
        url: post_url,
        attachment: attachments,
        tag: hashtag_tags(tx, post_id, domain).await?,
    });

    Ok(note)
}

pub async fn create_updated_note_from_post(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    post_id: Uuid,
    author_actor: &Actor,
    domain: &str,
    r2_public_endpoint_url: &str,
) -> Result<Note, AppError> {
    // Get post details
    let post = find_post_by_id(tx, post_id).await?;
    let post = post.ok_or_else(|| anyhow::anyhow!("Post not found"))?;

    // Get title and content
    let title = post.get("title").and_then(|t| t.as_ref()).map_or("", |v| v);
    let content = post
        .get("content")
        .and_then(|c| c.as_ref())
        .map_or("", |v| v);

    // Format content with title if available and process as markdown
    let formatted_content = if title.is_empty() {
        process_markdown_content(content)
    } else {
        let combined_content = format!("{}\n\n{}", title, content);
        process_markdown_content(&combined_content)
    };

    // Get attachments if image exists
    let mut attachments = Vec::new();
    if let Some(Some(image_id_str)) = post.get("image_id") {
        if let Ok(image_id) = Uuid::parse_str(image_id_str) {
            if let Ok(image) = find_image_by_id(tx, image_id).await {
                let image_url = format!(
                    "{}/image/{}{}/{}",
                    r2_public_endpoint_url,
                    image.image_filename.chars().next().unwrap_or('0'),
                    image.image_filename.chars().nth(1).unwrap_or('0'),
                    image.image_filename
                );

                let attachment = Attachment {
                    r#type: "Image".to_string(),
                    url: image_url,
                    media_type: "image/png".to_string(),
                    name: Some(title.to_string()),
                    width: Some(image.width),
                    height: Some(image.height),
                };
                attachments.push(attachment);
            }
        }
    }

    // Create URLs and IDs
    let post_url: Url =
        crate::models::post::post_page_url(domain, &author_actor.username, post_id).parse()?;

    let note_id: Url = format!("https://{}/ap/posts/{}", domain, post_id).parse()?;

    // Set up audience - public post
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", author_actor.iri)];

    // Get published date
    let published = post
        .get("published_at_utc")
        .ok_or_else(|| anyhow::anyhow!("Missing published_at_utc"))?;

    // Use current time for ActivityPub update timestamp
    let updated = chrono::Utc::now().to_rfc3339();

    // Create the Note object with updated timestamp
    let note = Note::from_params(NoteParams {
        id: note_id,
        attributed_to: ObjectId::<Actor>::parse(&author_actor.iri)?,
        content: formatted_content,
        to,
        cc,
        published: published.clone().unwrap_or_default(),
        updated: Some(updated),
        url: post_url,
        attachment: attachments,
        tag: hashtag_tags(tx, post_id, domain).await?,
    });

    Ok(note)
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
impl ActivityHandler for Delete {
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
                if let Err(e) = delete_post(&mut tx, post_id, PostDeletionReason::UserDeleted).await
                {
                    tracing::warn!(
                        "Failed to delete post {} from Delete activity: {:?}",
                        post_id,
                        e
                    );
                }
                tx.commit().await?;
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
                        if delete_comment_by_iri(&mut tx, &object_url).await? {
                            tracing::info!("Deleted comment with IRI: {}", object_url);
                            tx.commit().await?;
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
impl ActivityHandler for Like {
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
                                            // Get unread count for badge
                                            let badge_count =
                                                get_unread_count(&mut tx, recipient_id)
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
impl ActivityHandler for EmojiReact {
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
                                            // Get unread count for badge
                                            let badge_count =
                                                get_unread_count(&mut tx, recipient_id)
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

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNote {
    actor: ObjectId<Actor>,
    object: Note,
    r#type: UpdateType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl UpdateNote {
    pub fn new(
        actor: ObjectId<Actor>,
        object: Note,
        id: Url,
        to: Vec<String>,
        cc: Vec<String>,
        published: String,
    ) -> UpdateNote {
        UpdateNote {
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
impl ActivityHandler for UpdateNote {
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
        // UpdateNote activities notify followers about post content changes
        // In a full implementation, we would update our local copy of the post
        tracing::info!("Received UpdateNote activity: {:?}", self);
        Ok(())
    }
}
