//! Posts as notes, both ways: ours going out and theirs coming in.

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::{Activity, Object};

use activitystreams_kinds::activity::{CreateType, UpdateType};
use activitystreams_kinds::object::NoteType;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::live::LiveEvent;
use crate::markdown_utils::process_markdown_content;
use crate::models::actor::Actor;
use crate::models::comment::create_comment_from_activitypub;
use crate::models::image::find_image_by_id;
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::sanitized_html::SanitizedHtml;
use crate::web::state::AppState;

use super::{content_or_contents_deser, string_or_vec_deser, tag_or_vec_deser};

fn extract_note_content(note: &Note) -> (String, Option<SanitizedHtml>) {
    // Try to get HTML content from contents field or content field
    let raw_html_content = note.content.clone();

    // Sanitize HTML content if present using ammonia defaults
    let html_content = raw_html_content.map(|html| {
        let sanitized = SanitizedHtml::clean(&html);

        tracing::debug!(
            "Sanitized HTML content: original length {}, sanitized length {}",
            html.len(),
            sanitized.as_str().len()
        );

        if html != sanitized.as_str() {
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
                        .map_or_else(|| "No content".to_string(), SanitizedHtml::into_string)
                }
            } else {
                // Fallback to sanitized HTML content if available, or "No content"
                html_content
                    .clone()
                    .map_or_else(|| "No content".to_string(), SanitizedHtml::into_string)
            }
        } else {
            // Fallback to sanitized HTML content if available, or "No content"
            html_content
                .clone()
                .map_or_else(|| "No content".to_string(), SanitizedHtml::into_string)
        }
    } else {
        // No source field, use sanitized HTML content as fallback for markdown too
        html_content
            .clone()
            .map_or_else(|| "No content".to_string(), SanitizedHtml::into_string)
    };

    (markdown_content, html_content)
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
    /// The Group a post belongs to (FEP-1b12), which is how Lemmy, Mbin and
    /// the like tell a community's post from its author's own. Read leniently:
    /// a Note whose `audience` is not a single IRI is still a Note.
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "lenient_url_deser"
    )]
    audience: Option<Url>,
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
}

fn lenient_url_deser<'de, D>(deserializer: D) -> Result<Option<Url>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value
        .as_ref()
        .and_then(|value| value.as_str())
        .and_then(|iri| Url::parse(iri).ok()))
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
    fn hashtag(domain: &str, tag: &crate::models::tag::PostTag) -> Option<Tag> {
        Some(Tag {
            r#type: "Hashtag".to_string(),
            href: format!("https://{}/tags/{}", domain, urlencoding::encode(&tag.name))
                .parse()
                .ok(),
            // With the `#`, which is what every implementation expects to read
            // and what the name in the database deliberately does not carry.
            name: Some(format!("#{}", tag.display_name)),
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
    Ok(crate::models::tag::get_tags_for_post(tx, post_id)
        .await?
        .iter()
        .filter_map(|tag| Tag::hashtag(domain, tag))
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
    pub audience: Option<Url>,
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
            audience: params.audience,
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
impl Activity for Create {
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
                                data.live.publish(LiveEvent::Comments { post_id, by: None });

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

pub fn generate_object_id(domain: &str) -> Result<Url, AppError> {
    Ok(Url::parse(&format!(
        "https://{}/objects/{}",
        domain,
        Uuid::new_v4()
    ))?)
}

/// A post's Note `url` and `audience`: the page `post_page_path` gives it,
/// and the Group of the community it is in, if any. Both change when the post
/// moves, which is why a move sends an `Update`.
async fn note_page_and_audience(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    post: &std::collections::HashMap<String, Option<String>>,
    post_id: Uuid,
    author_actor: &Actor,
    domain: &str,
) -> Result<(Url, Option<Url>), AppError> {
    let community_slug = post.get("community_slug").and_then(|v| v.as_deref());
    let post_url =
        crate::models::post::post_page_url(domain, &author_actor.username, community_slug, post_id)
            .parse()?;
    let audience = match post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        Some(community_id) => Actor::find_by_community_id(tx, community_id)
            .await?
            .map(|group| group.iri.url().clone()),
        None => None,
    };
    Ok((post_url, audience))
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
    let (post_url, audience) =
        note_page_and_audience(tx, &post, post_id, author_actor, domain).await?;

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
        audience,
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
    let (post_url, audience) =
        note_page_and_audience(tx, &post, post_id, author_actor, domain).await?;

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
        audience,
    });

    Ok(note)
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
impl Activity for UpdateNote {
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
