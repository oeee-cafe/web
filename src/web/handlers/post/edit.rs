//! Changing a post after it is up: its community, its text, or deleting it.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::community::choice::{
    choose_communities, ChoiceQuery, ChoiceRule, CommunityChoice,
};
use crate::models::community::{find_community_by_id, CommunityVisibility};
use crate::models::image::find_image_by_id;
use crate::models::post::{
    delete_post_with_activity, edit_post, edit_post_community, find_post_by_id,
    get_draft_post_count, is_post_movable,
};
use crate::models::tag::{get_tags_for_post, set_post_tags};
use crate::models::user::{find_user_by_id, AuthSession};
use crate::web::context::CommonContext;
use crate::web::htmx::is_htmx;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use anyhow::Error;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, response::Html, Form};
use minijinja::context;
use serde::Deserialize;
use std::collections::HashMap;
use uuid::Uuid;

use super::federation::{community_federates, send_post_update_to_followers};
use super::get_community_slug_url;
use minijinja::value::Serde;

/// How many communities the move page lists beyond the mover's own, most
/// active first. The rest are a search away.
const MOVE_OTHERS_LIMIT: i64 = 30;

#[derive(Deserialize)]
pub struct MoveSearchQuery {
    #[serde(default)]
    pub q: String,
}

/// The move page, and with `HX-Request` just its list, which the search box
/// asks for as you type. The list is narrowed in the database: there are too
/// many public communities to send each with its three drawings and filter
/// them in the browser.
pub async fn post_edit_community(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((_login_name, id)): Path<(String, String)>,
    Query(search): Query<MoveSearchQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing author_id".to_string()))?
        != user_id.to_string()
    {
        return Err(AppError::Forbidden);
    }

    // Not part of a thread, and not in a private or two-tone community.
    if !is_post_movable(&mut tx, post_uuid).await? {
        return Err(AppError::Forbidden);
    }

    let current_community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());
    let current_community = match current_community_id {
        Some(community_id) => find_community_by_id(&mut tx, community_id).await?,
        None => None,
    };

    let query = search.q.trim();
    // One past the limit, to say whether there are more than the list shows.
    let mut destinations = choose_communities(
        &mut tx,
        &ChoiceQuery {
            exclude: current_community_id,
            ..ChoiceQuery::new(
                ChoiceRule::Move,
                Some(user_id),
                query,
                MOVE_OTHERS_LIMIT + 1,
            )
        },
    )
    .await?;

    // Three latest drawings a row, for every row in one query.
    let ids: Vec<Uuid> = destinations.iter().map(|c| c.id).collect();
    let recent_posts = sqlx::query!(
        r#"
        SELECT ranked.community_id AS "community_id!", ranked.image_filename
        FROM (
            SELECT
                p.community_id,
                i.image_filename,
                ROW_NUMBER() OVER (PARTITION BY p.community_id ORDER BY p.published_at DESC) AS rn
            FROM posts p
            INNER JOIN images i ON p.image_id = i.id
            WHERE p.community_id = ANY($1)
                AND p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
        ) ranked
        WHERE ranked.rn <= 3
        ORDER BY ranked.community_id, ranked.rn
        "#,
        &ids
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut posts_by_community: HashMap<Uuid, Vec<serde_json::Value>> = HashMap::new();
    for row in recent_posts {
        posts_by_community
            .entry(row.community_id)
            .or_default()
            .push(serde_json::json!({ "image_filename": row.image_filename }));
    }

    let others_total = destinations.iter().filter(|c| !c.is_mine).count();
    let has_more = others_total as i64 > MOVE_OTHERS_LIMIT;
    if has_more {
        destinations.truncate(destinations.len() - 1);
    }

    let row = |c: &CommunityChoice| {
        serde_json::json!({
            "id": c.id.to_string(),
            "name": c.name,
            "slug": c.slug,
            "owner_login_name": c.owner_login_name,
            "posts_count": c.posts_count,
            "recent_posts": posts_by_community.get(&c.id).cloned().unwrap_or_default(),
        })
    };
    let unlisted_communities: Vec<_> = destinations
        .iter()
        .filter(|c| c.visibility == CommunityVisibility::Unlisted)
        .map(row)
        .collect();
    let public_participated_communities: Vec<_> = destinations
        .iter()
        .filter(|c| c.visibility == CommunityVisibility::Public && c.has_participated)
        .map(row)
        .collect();
    let public_other_communities: Vec<_> = destinations
        .iter()
        .filter(|c| c.visibility == CommunityVisibility::Public && !c.has_participated)
        .map(row)
        .collect();

    let ctx = context! {
        post,
        post_id => id,
        current_community => Serde(current_community.map(|c| serde_json::json!({
            "name": c.name,
            "slug": c.slug,
        }))),
        unlisted_communities => Serde(unlisted_communities),
        public_participated_communities => Serde(public_participated_communities),
        public_other_communities => Serde(public_other_communities),
        has_more,
        query,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    };

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;
    tx.commit().await?;

    let template = "post_edit_community.jinja";
    let rendered = if is_htmx(&headers) {
        state
            .render_page_block(template, "move_list", common_ctx, ctx)
            .await?
    } else {
        state.render_page(template, common_ctx, ctx).await?
    };

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
    if !is_post_movable(&mut tx, post_uuid).await? {
        // Post cannot be moved (part of thread, in private/two-tone community)
        return Err(AppError::Forbidden);
    }

    // The form only lists communities a post may go to, but the id is the
    // request's own. None is the author's own page, which is always allowed.
    if let Some(community_id) = form.community_id
        && !crate::models::post::may_move_post_into(&mut tx, community_id).await?
    {
        return Err(AppError::Forbidden);
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
    if let Some(actor) = author_actor
        && published
        && community_federates(destination.as_ref())
        && let Err(e) = send_post_update_to_followers(&actor, post_uuid, &state).await
    {
        tracing::error!("Failed to federate a post's move: {:?}", e);
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
                current_user => Serde(auth_session.user),
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
    if let Some(actor) = actor
        && let Some(ref post_data) = post
    {
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
                tracing::info!("Skipping ActivityPub federation for private community post update");
            }
        }
    }

    let template = "post_view.jinja";
    let rendered = state
        .render_block(
            template,
            "post_edit_block",
            context! {
                current_user => Serde(auth_session.user),
                    post,
                post_id => id,
                tags => Serde(tags),
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
