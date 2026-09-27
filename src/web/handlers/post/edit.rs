//! Changing a post after it is up: its community, its text, or deleting it.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::community::{find_community_by_id, get_known_communities};
use crate::models::image::find_image_by_id;
use crate::models::post::{
    delete_post_with_activity, edit_post, edit_post_community, find_post_by_id,
    get_draft_post_count,
};
use crate::models::tag::{get_tags_for_post, set_post_tags};
use crate::models::user::{find_user_by_id, AuthSession};
use crate::web::context::CommonContext;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use anyhow::Error;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, response::Html, Form};
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use super::federation::{community_federates, send_post_update_to_followers};
use super::get_community_slug_url;

pub async fn post_edit_community(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((_login_name, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    // Check if post is movable using the helper function
    use crate::models::post::is_post_movable;
    if !is_post_movable(&mut tx, post_uuid).await? {
        // Post cannot be moved (part of thread, in private/two-tone community)
        return Err(AppError::Forbidden);
    }

    let post_data = post.clone();
    let current_community_id = post_data
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    // Get current community details with owner info (if post is in a community)
    let (current_community_result, current_community_recent_posts) = if let Some(comm_id) =
        current_community_id
    {
        let community_result = sqlx::query!(
            r#"
            SELECT
                c.id, c.owner_id, c.name, c.slug, c.description,
                c.visibility as "visibility: crate::models::community::CommunityVisibility", c.updated_at, c.created_at, c.background_color, c.foreground_color,
                u.login_name AS "owner_login_name?"
            FROM communities c
            LEFT JOIN users u ON c.owner_id = u.id
            WHERE c.id = $1
            "#,
            comm_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        // Get recent posts for current community
        let current_posts = sqlx::query!(
            r#"
            SELECT
                p.id,
                i.image_filename,
                i.width as image_width,
                i.height as image_height,
                u.login_name as author_login_name
            FROM posts p
            INNER JOIN images i ON p.image_id = i.id
            INNER JOIN users u ON p.author_id = u.id
            WHERE p.community_id = $1
                AND p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
            ORDER BY p.published_at DESC
            LIMIT 3
            "#,
            comm_id
        )
        .fetch_all(&mut *tx)
        .await?;

        let posts: Vec<serde_json::Value> = current_posts
            .into_iter()
            .map(|post| {
                serde_json::json!({
                    "id": post.id.to_string(),
                    "image_filename": post.image_filename,
                    "image_width": post.image_width,
                    "image_height": post.image_height,
                    "author_login_name": post.author_login_name,
                })
            })
            .collect();

        (community_result, posts)
    } else {
        (None, Vec::new())
    };

    let current_community = current_community_result.map(|row| {
        serde_json::json!({
            "id": row.id.to_string(),
            "owner_id": row.owner_id.to_string(),
            "name": row.name,
            "slug": row.slug,
            "description": row.description,
            "visibility": row.visibility,
            "owner_login_name": row.owner_login_name.unwrap_or_else(|| String::from("")),
            "recent_posts": current_community_recent_posts,
        })
    });

    // Fetch both public and known communities
    let public_communities = crate::models::community::get_public_communities(&mut tx).await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let known_communities = get_known_communities(&mut tx, user_id).await?;

    // Get communities the user has participated in
    let participated_community_ids: std::collections::HashSet<Uuid> = sqlx::query!(
        r#"SELECT DISTINCT community_id FROM posts WHERE author_id = $1 AND community_id IS NOT NULL"#,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .filter_map(|row| row.community_id)
    .collect();

    // Separate known and public-only communities, filtering out current community
    use std::collections::HashSet;
    let known_ids: HashSet<Uuid> = known_communities.iter().map(|c| c.id).collect();

    // Get all community IDs for fetching recent posts
    let mut all_community_ids: Vec<Uuid> = Vec::new();
    for c in &known_communities {
        if current_community_id.map_or(true, |curr_id| c.id != curr_id) {
            all_community_ids.push(c.id);
        }
    }
    for c in &public_communities {
        if current_community_id.map_or(true, |curr_id| c.id != curr_id)
            && !known_ids.contains(&c.id)
        {
            all_community_ids.push(c.id);
        }
    }

    // Fetch recent posts (3 per community) for all communities in a single batch query
    let recent_posts = if !all_community_ids.is_empty() {
        sqlx::query!(
            r#"
            SELECT
                ranked.id,
                ranked.community_id,
                ranked.image_filename,
                ranked.image_width,
                ranked.image_height,
                ranked.author_login_name
            FROM (
                SELECT
                    p.id,
                    p.community_id,
                    i.image_filename,
                    i.width as image_width,
                    i.height as image_height,
                    u.login_name as author_login_name,
                    ROW_NUMBER() OVER (PARTITION BY p.community_id ORDER BY p.published_at DESC) as rn
                FROM posts p
                INNER JOIN images i ON p.image_id = i.id
                INNER JOIN users u ON p.author_id = u.id
                WHERE p.community_id = ANY($1)
                    AND p.published_at IS NOT NULL
                    AND p.deleted_at IS NULL
            ) ranked
            WHERE ranked.rn <= 3
            ORDER BY ranked.community_id, ranked.rn
            "#,
            &all_community_ids
        )
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };

    // Group posts by community_id (already limited to 3 per community by the query)
    use std::collections::HashMap;
    let mut posts_by_community: HashMap<Uuid, Vec<serde_json::Value>> = HashMap::new();
    for post in recent_posts {
        if let Some(community_id) = post.community_id {
            let posts = posts_by_community.entry(community_id).or_default();
            posts.push(serde_json::json!({
                "id": post.id.to_string(),
                "image_filename": post.image_filename,
                "image_width": post.image_width,
                "image_height": post.image_height,
                "author_login_name": post.author_login_name,
            }));
        }
    }

    // Combine all communities (known + public), filtering out current and two-tone communities
    let mut all_communities: Vec<_> = Vec::new();

    // Add known communities
    for c in known_communities {
        if let Some(curr_id) = current_community_id {
            if c.id == curr_id {
                continue;
            }
        }
        // Filter out two-tone communities
        if c.background_color.is_some() && c.foreground_color.is_some() {
            continue;
        }
        // Filter out private communities - moving to private communities is restricted
        if matches!(
            c.visibility,
            crate::models::community::CommunityVisibility::Private
        ) {
            continue;
        }

        let recent_posts = posts_by_community.get(&c.id).cloned().unwrap_or_default();
        let has_participated = participated_community_ids.contains(&c.id);
        all_communities.push(serde_json::json!({
            "id": c.id.to_string(),
            "name": c.name,
            "slug": c.slug,
            "description": c.description,
            "visibility": c.visibility,
            "owner_login_name": c.owner_login_name,
            "posts_count": null,
            "recent_posts": recent_posts,
            "has_participated": has_participated,
        }));
    }

    // Add public communities (that aren't already in known)
    for c in public_communities {
        if let Some(curr_id) = current_community_id {
            if c.id == curr_id {
                continue;
            }
        }
        if known_ids.contains(&c.id) {
            continue;
        }
        // Filter out two-tone communities
        if c.background_color.is_some() && c.foreground_color.is_some() {
            continue;
        }

        let recent_posts = posts_by_community.get(&c.id).cloned().unwrap_or_default();
        let has_participated = participated_community_ids.contains(&c.id);
        all_communities.push(serde_json::json!({
            "id": c.id.to_string(),
            "name": c.name,
            "slug": c.slug,
            "description": c.description,
            "visibility": c.visibility,
            "owner_login_name": c.owner_login_name,
            "posts_count": c.posts_count,
            "recent_posts": recent_posts,
            "has_participated": has_participated,
        }));
    }

    // Separate by visibility type and participation
    let mut unlisted_communities: Vec<_> = all_communities
        .iter()
        .filter(|c| c.get("visibility").and_then(|v| v.as_str()) == Some("unlisted"))
        .cloned()
        .collect();

    let mut public_participated_communities: Vec<_> = all_communities
        .iter()
        .filter(|c| {
            c.get("visibility").and_then(|v| v.as_str()) == Some("public")
                && c.get("has_participated").and_then(|v| v.as_bool()) == Some(true)
        })
        .cloned()
        .collect();

    let mut public_other_communities: Vec<_> = all_communities
        .iter()
        .filter(|c| {
            c.get("visibility").and_then(|v| v.as_str()) == Some("public")
                && c.get("has_participated").and_then(|v| v.as_bool()) == Some(false)
        })
        .cloned()
        .collect();

    // Sort each list alphabetically by name
    let sort_by_name = |a: &serde_json::Value, b: &serde_json::Value| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    };

    unlisted_communities.sort_by(sort_by_name);
    public_participated_communities.sort_by(sort_by_name);
    public_other_communities.sort_by(sort_by_name);

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    tx.commit().await?;

    let template = "post_edit_community.jinja";
    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                post,
                post_id => id,
                current_community,
                unlisted_communities,
                public_participated_communities,
                public_other_communities,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
                base_url => state.config.base_url.clone(),
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

fn deserialize_empty_string_as_none<'de, D>(deserializer: D) -> Result<Option<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(s) if s.is_empty() => Ok(None),
        Some(s) => Uuid::parse_str(&s)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

#[derive(Deserialize)]
pub struct EditPostCommunityForm {
    #[serde(deserialize_with = "deserialize_empty_string_as_none")]
    pub community_id: Option<Uuid>,
}

pub async fn do_post_edit_community(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((_login_name, id)): Path<(String, String)>,
    Form(form): Form<EditPostCommunityForm>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    // Check if post is movable using the helper function
    use crate::models::post::is_post_movable;
    if !is_post_movable(&mut tx, post_uuid).await? {
        // Post cannot be moved (part of thread, in private/two-tone community)
        return Err(AppError::Forbidden);
    }

    // The form only lists communities a post may go to, but the id is the
    // request's own. None is the author's own page, which is always allowed.
    if let Some(community_id) = form.community_id {
        if !crate::models::post::may_move_post_into(&mut tx, community_id).await? {
            return Err(AppError::Forbidden);
        }
    }

    // Not `let _ =`: a move that failed was redirected to as if it had worked.
    edit_post_community(&mut tx, post_uuid, form.community_id).await?;

    let destination = match form.community_id {
        Some(community_id) => find_community_by_id(&mut tx, community_id).await?,
        None => None,
    };
    let author_actor = Actor::find_by_user_id(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
    )
    .await?;
    let author_login_name = post
        .get("login_name")
        .and_then(|v| v.clone())
        .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;
    tx.commit().await?;

    // The move changes the post's page, which is the Note's `url`, and its
    // `audience`. Servers that hold the Note are told, so their "open
    // original" link follows it; one that misses this still lands on the page
    // through the redirect from the old address.
    let published = post.get("published_at").and_then(|p| p.as_ref()).is_some();
    if let Some(actor) = author_actor {
        if published && community_federates(destination.as_ref()) {
            if let Err(e) = send_post_update_to_followers(&actor, post_uuid, &state).await {
                tracing::error!("Failed to federate a post's move: {:?}", e);
            }
        }
    }

    Ok(Redirect::to(&crate::models::post::post_page_path(
        &author_login_name,
        destination
            .as_ref()
            .map(|community| community.slug.as_str()),
        post_uuid,
    ))
    .into_response())
}

pub async fn hx_edit_post(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;

    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    // Get existing tags for this post
    let tags = get_tags_for_post(&mut tx, post_uuid)
        .await
        .unwrap_or_default();
    let tags_string = tags
        .iter()
        .map(|h| h.display_name.clone())
        .collect::<Vec<_>>()
        .join(", ");

    tx.commit().await?;

    let rendered = state
        .render(
            "post_edit.jinja",
            context! {
                current_user => auth_session.user,
                post,
                post_id => id,
                tags => tags_string,
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct EditPostForm {
    pub title: String,
    pub content: String,
    pub is_sensitive: Option<String>,
    pub allow_relay: Option<String>,
    pub allow_replay: Option<String>,
    pub tags: Option<String>,
}

pub async fn hx_do_edit_post(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<EditPostForm>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    let _ = edit_post(
        &mut tx,
        post_uuid,
        form.title.clone(),
        form.content.clone(),
        form.is_sensitive == Some("on".to_string()),
        form.allow_relay == Some("on".to_string()),
        form.allow_replay == Some("on".to_string()),
    )
    .await;

    set_post_tags(&mut tx, post_uuid, form.tags.as_deref()).await?;

    let post = find_post_by_id(&mut tx, post_uuid).await?;

    // Get tags for this post
    let tags = get_tags_for_post(&mut tx, post_uuid)
        .await
        .unwrap_or_default();

    // Find the actor for this user to send ActivityPub activities
    let actor = Actor::find_by_user_id(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
    )
    .await?;

    // Check community visibility before federating updates. A personal post
    // federates as it did when it was published.
    let should_federate = match post
        .as_ref()
        .and_then(|post_data| post_data.get("community_id"))
        .and_then(|v| v.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        Some(community_id) => {
            community_federates(find_community_by_id(&mut tx, community_id).await?.as_ref())
        }
        None => post.is_some(),
    };

    let _ = tx.commit().await;

    // Send ActivityPub Update activity to followers if actor exists and post is published
    // For public and unlisted communities (not private)
    if let Some(actor) = actor {
        if let Some(ref post_data) = post {
            // Only send ActivityPub activities for published posts
            if post_data
                .get("published_at")
                .and_then(|p| p.as_ref())
                .is_some()
            {
                if should_federate {
                    // Send update to user's followers
                    if let Err(e) = send_post_update_to_followers(&actor, post_uuid, &state).await {
                        tracing::error!(
                            "Failed to send post update to user's ActivityPub followers: {:?}",
                            e
                        );
                        // Don't fail the entire operation if ActivityPub sending fails
                    }
                } else {
                    tracing::info!(
                        "Skipping ActivityPub federation for private community post update"
                    );
                }
            }
        }
    }

    let template = "post_view.jinja";
    let rendered = state
        .render_block(
            template,
            "post_edit_block",
            context! {
                current_user => auth_session.user,
                    post,
                post_id => id,
                tags,
                ftl_lang
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

/// `?in_place=1`: the page asking stays where it is -- the drafts list takes
/// the card away itself, and is told what else that changes
/// (draft_delete_oob.jinja) -- instead of being sent to where the post lived.
#[derive(Deserialize, Default)]
pub struct DeletePostQuery {
    in_place: Option<String>,
}

pub async fn hx_delete_post(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DeletePostQuery>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Err(AppError::Forbidden);
    }

    let image_id = post
        .get("image_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing image_id".to_string()))
        .and_then(|id_str| {
            Uuid::parse_str(id_str)
                .map_err(|e| AppError::BadRequest(format!("Invalid UUID {}: {}", id_str, e)))
        })?;
    let image = find_image_by_id(&mut tx, image_id).await?;

    let mut keys = vec![format!(
        "image/{}{}/{}",
        image
            .image_filename
            .chars()
            .next()
            .ok_or_else(|| AppError::BadRequest("Image filename too short".to_string()))?,
        image
            .image_filename
            .chars()
            .nth(1)
            .ok_or_else(|| AppError::BadRequest("Image filename too short".to_string()))?,
        image.image_filename
    )];

    // Only add replay file to deletion if it exists
    if let Some(ref replay_filename) = image.replay_filename {
        keys.push(format!(
            "replay/{}{}/{}",
            replay_filename
                .chars()
                .next()
                .ok_or_else(|| AppError::BadRequest("Replay filename too short".to_string()))?,
            replay_filename
                .chars()
                .nth(1)
                .ok_or_else(|| AppError::BadRequest("Replay filename too short".to_string()))?,
            replay_filename
        ));
    }

    let credentials: AwsCredentials = AwsCredentials::new(
        state.config.aws_access_key_id.clone(),
        state.config.aws_secret_access_key.clone(),
        None,
        None,
        "",
    );
    let credentials_provider = SharedCredentialsProvider::new(credentials);
    let config = aws_sdk_s3::Config::builder()
        .endpoint_url(state.config.r2_endpoint_url.clone())
        .region(Region::new(state.config.aws_region.clone()))
        .credentials_provider(credentials_provider)
        .behavior_version_latest()
        .build();
    let client = Client::from_conf(config);
    let objects: Vec<ObjectIdentifier> = keys
        .iter()
        .map(|key| ObjectIdentifier::builder().key(key).build())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            AppError::from(anyhow::anyhow!("Failed to build object identifiers: {}", e))
        })?;

    client
        .delete_objects()
        .bucket(state.config.aws_s3_bucket.clone())
        .delete(
            Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(Error::from)?,
        )
        .send()
        .await?;
    let post_data = post.clone();
    // A draft goes back to the drafts it was one of; a published post to
    // where it lived.
    let is_draft = post_data
        .get("published_at")
        .and_then(|v| v.as_ref())
        .is_none();
    let redirect_url = if is_draft {
        "/posts/drafts".to_string()
    } else if let Some(community_id_str) = post_data.get("community_id").and_then(|id| id.clone()) {
        let community_id = Uuid::parse_str(&community_id_str)?;
        get_community_slug_url(&mut tx, community_id).await?
    } else {
        // For personal posts, redirect to user's profile
        let author_id = post_data
            .get("author_id")
            .and_then(|v| v.as_ref())
            .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?;
        let author = find_user_by_id(&mut tx, Uuid::parse_str(author_id)?).await?;
        format!(
            "/@{}",
            author
                .ok_or_else(|| AppError::NotFound("Author".to_string()))?
                .login_name
        )
    };

    // The tags stay on the post. Deletion here is soft, and a tag counts and
    // lists only undeleted posts, so there is nothing to decrement and a post
    // that comes back comes back tagged.
    // A draft was never announced, so there is nothing for followers to
    // take back: a Delete for it would only tell them it had existed.
    let was_published = post.get("published_at").and_then(|v| v.as_ref()).is_some();
    let falls =
        delete_post_with_activity(&mut tx, post_uuid, was_published.then_some(&state)).await?;
    let remaining = match (&query.in_place, &auth_session.user) {
        (Some(_), Some(user)) => Some(get_draft_post_count(&mut tx, user.id).await?),
        _ => None,
    };
    tx.commit().await?;
    state.push_service.badges_fell(falls);

    if let Some(remaining) = remaining {
        let rendered = state
            .render("draft_delete_oob.jinja", context! { remaining, ftl_lang })
            .await?;
        return Ok(Html(rendered).into_response());
    }
    Ok(([("HX-Redirect", &redirect_url)],).into_response())
}
