//! Posts as notes, both ways: ours going out and theirs coming in.

use activitystreams_kinds::activity::{CreateType, UpdateType};
use activitystreams_kinds::object::NoteType;
use ojak::federation::Uris;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::markdown_utils::process_markdown_content;
use crate::models::actor::Actor;
use crate::models::image::find_image_by_id;
use crate::models::post::find_post_by_id;

use super::{content_or_contents_deser, string_or_vec_deser, tag_or_vec_deser};

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
    attributed_to: Option<Url>,
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
    pub attributed_to: Url,
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
    actor: Url,
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
        actor: Url,
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
    uris: &Uris,
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
    if let Some(Some(image_id_str)) = post.get("image_id")
        && let Ok(image_id) = Uuid::parse_str(image_id_str)
        && let Ok(image) = find_image_by_id(tx, image_id).await
    {
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

    // Create URLs and IDs
    let (post_url, audience) =
        note_page_and_audience(tx, &post, post_id, author_actor, domain).await?;

    let note_id = uris.object_uri("note", &[("post_id", &post_id.to_string())])?;

    // Set up audience - public post
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", author_actor.iri)];

    // Get published date
    let published = post
        .get("published_at_utc")
        .ok_or_else(|| anyhow::anyhow!("Missing published_at_utc"))?;

    let note = Note::from_params(NoteParams {
        id: note_id,
        attributed_to: author_actor.iri.url().clone(),
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
    uris: &Uris,
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
    if let Some(Some(image_id_str)) = post.get("image_id")
        && let Ok(image_id) = Uuid::parse_str(image_id_str)
        && let Ok(image) = find_image_by_id(tx, image_id).await
    {
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

    // Create URLs and IDs
    let (post_url, audience) =
        note_page_and_audience(tx, &post, post_id, author_actor, domain).await?;

    let note_id = uris.object_uri("note", &[("post_id", &post_id.to_string())])?;

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
        attributed_to: author_actor.iri.url().clone(),
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
    actor: Url,
    object: Note,
    r#type: UpdateType,
    id: Url,
    to: Vec<String>,
    cc: Vec<String>,
    published: String,
}

impl UpdateNote {
    pub fn new(
        actor: Url,
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
