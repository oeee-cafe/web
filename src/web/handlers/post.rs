use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::comment::{
    build_comment_thread_tree, create_comment, extract_mentions, find_users_by_login_names,
    CommentDraft,
};
use crate::models::community::{
    find_community_by_id, get_known_communities, get_user_role_in_community, is_user_member,
};
use crate::models::follow;
use crate::models::hashtag::{get_hashtags_for_post, set_post_hashtags};
use crate::models::image::find_image_by_id;
use crate::models::notification::{
    create_notification, get_notification_by_id, get_unread_count, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::{
    build_thread_tree, delete_post_with_activity, edit_post, edit_post_community,
    find_draft_posts_by_author_id, find_post_by_id, increment_post_viewer_count, publish_post,
    SerializableThreadedPost,
};
use crate::models::reaction::{
    create_reaction, delete_reaction, find_reactions_by_post_id, get_reaction_counts, ReactionDraft,
};
use crate::models::user::{find_user_by_id, AuthSession, Language};
use crate::web::context::CommonContext;
use crate::web::handlers::activitypub::{
    create_note_from_post, create_updated_note_from_post, generate_object_id, Announce, Create,
    Note, UpdateNote,
};
use crate::web::handlers::{
    get_bundle, handler_404, parse_id_with_legacy_support, safe_get_message, ExtractFtlLang,
    ParsedId,
};
use crate::web::state::AppState;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::Actor as ActivityPubActor;
use anyhow::Error;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Json, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, Form};
use axum_messages::Messages;
use minijinja::context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use urlencoding;
use uuid::Uuid;

// Helper function to get community @slug URL from UUID
async fn get_community_slug_url(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    community_id: uuid::Uuid,
) -> Result<String, AppError> {
    let community = find_community_by_id(tx, community_id).await?;
    if let Some(community) = community {
        Ok(format!("/communities/@{}", community.slug))
    } else {
        Ok(format!("/communities/{}", community_id)) // Fallback to UUID if community not found
    }
}

/// Helper function to show a flash error message and redirect
fn flash_error_and_redirect(
    headers: &HeaderMap,
    user_preferred_language: Option<Language>,
    messages: Messages,
    message_key: &str,
    redirect_path: &str,
) -> axum::response::Response {
    let accept_language = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .cloned()
        .unwrap_or_else(|| axum::http::HeaderValue::from_static(""));
    let bundle = get_bundle(&accept_language, user_preferred_language);
    let error_message = safe_get_message(&bundle, message_key);
    messages.error(error_message);
    Redirect::to(redirect_path).into_response()
}

/// Helper function to redirect unauthenticated users to login with next parameter
fn redirect_to_login(current_path: &str) -> axum::response::Response {
    let login_url = format!("/login?next={}", urlencoding::encode(current_path));
    Redirect::to(&login_url).into_response()
}

async fn send_post_to_followers(
    actor: &Actor,
    post_id: Uuid,
    _title: String,
    _content: String,
    state: &AppState,
) -> Result<Note, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get all followers for this actor
    let followers = follow::find_followers_by_actor_id(&mut tx, actor.id).await?;

    // Use the shared function to create the Note
    let note = create_note_from_post(
        &mut tx,
        post_id,
        actor,
        &state.config.domain,
        &state.config.r2_public_endpoint_url,
    )
    .await?;

    let actor_object_id = ObjectId::parse(&actor.iri)?;
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];
    let published = chrono::Utc::now().to_rfc3339();

    tracing::info!("Note: {:?}", note);

    // Only send to followers if there are any
    if !followers.is_empty() {
        // Create the Create activity
        let activity_id = generate_object_id(&state.config.domain)?;
        let create = Create::new(
            actor_object_id,
            note.clone(),
            activity_id,
            to,
            cc,
            published,
        );

        // Get follower inboxes
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // For now, we'll create a minimal federation config to send activities
            // In a production setup, this would be properly integrated with the federation middleware
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all followers
            actor
                .send(
                    create,
                    follower_inboxes,
                    state.config.use_activitypub_queue(),
                    &federation_data,
                )
                .await?;
            tracing::info!(
                "Sent Create activity for post {} to {} followers",
                post_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No followers found for actor {}, skipping ActivityPub post",
            actor.iri
        );
    }

    tx.commit().await?;
    Ok(note)
}

async fn send_post_to_community_followers(
    _user_actor: &Actor,
    community_id: Uuid,
    note: &Note,
    state: &AppState,
) -> Result<(), AppError> {
    // Print note
    tracing::info!("Note: {:?}", note);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the community's actor, create one if it doesn't exist
    let mut community_actor = Actor::find_by_community_id(&mut tx, community_id).await?;
    if community_actor.is_none() {
        // Community doesn't have an actor yet, create one
        let community = find_community_by_id(&mut tx, community_id).await?;
        if let Some(community) = community {
            tracing::info!(
                "Creating actor for community {} as it doesn't exist",
                community_id
            );
            match crate::models::actor::create_actor_for_community(
                &mut tx,
                &community,
                &state.config,
            )
            .await
            {
                Ok(new_actor) => community_actor = Some(new_actor),
                Err(e) => {
                    tracing::error!(
                        "Failed to create actor for community {}: {:?}",
                        community_id,
                        e
                    );
                    return Ok(());
                }
            }
        } else {
            tracing::error!("Community {} not found", community_id);
            return Ok(());
        }
    }

    if let Some(community_actor) = community_actor {
        // Get all followers for the community actor
        let followers = follow::find_followers_by_actor_id(&mut tx, community_actor.id).await?;

        if followers.is_empty() {
            tracing::info!(
                "No followers found for community actor {}, skipping ActivityPub post",
                community_actor.iri
            );
            tx.commit().await?;
            return Ok(());
        }

        // Create the Announce activity referencing the user's original note
        let note_id = note.id.clone();
        let community_actor_object_id = ObjectId::<Actor>::parse(&community_actor.iri)?;

        let published = chrono::Utc::now().to_rfc3339();

        // For the Announce activity, the audience should be the community's followers
        let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
        let cc = vec![
            format!("{}/followers", community_actor.iri), // Community's followers
        ];

        // Create the Announce activity where the community announces the user's post
        let announce_activity_id = generate_object_id(&state.config.domain)?;
        let announce = Announce::new(
            community_actor_object_id,
            note_id.clone(), // The URL of the original post being announced
            announce_activity_id,
            to.clone(),
            cc.clone(),
            published.clone(),
        );

        // Get follower inboxes (community followers)
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // Create federation config to send activities
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all community followers using the community actor (announcing the user's post)
            community_actor
                .send(
                    announce,
                    follower_inboxes,
                    state.config.use_activitypub_queue(),
                    &federation_data,
                )
                .await?;
            tracing::info!(
                "Sent Announce activity for note {} to {} community followers",
                note_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No actor found for community {}, skipping community followers ActivityPub",
            community_id
        );
    }

    tx.commit().await?;
    Ok(())
}

/// Whether the author left this drawing open to being relayed.
///
/// The post page only offers the button when they did. This is that same rule,
/// applied to whoever arrives at the URL another way.
fn relay_is_allowed(post: &std::collections::HashMap<String, Option<String>>) -> bool {
    post.get("allow_relay").and_then(|value| value.as_deref()) == Some("true")
}

#[cfg(test)]
mod relay_permission_tests {
    use super::relay_is_allowed;
    use std::collections::HashMap;

    fn post(allow_relay: Option<&str>) -> HashMap<String, Option<String>> {
        let mut post = HashMap::new();
        if let Some(allow_relay) = allow_relay {
            post.insert("allow_relay".to_string(), Some(allow_relay.to_string()));
        }
        post
    }

    #[test]
    fn only_a_drawing_left_open_may_be_relayed() {
        assert!(relay_is_allowed(&post(Some("true"))));
        assert!(!relay_is_allowed(&post(Some("false"))));
        // The column is not null, so a post always carries the flag -- but a
        // missing one must not read as permission.
        assert!(!relay_is_allowed(&post(None)));
    }
}

/// Turned away from a drawing whose author closed it to relays.
fn relay_closed(
    headers: &HeaderMap,
    auth_session: &AuthSession,
    messages: Messages,
    post_id: Uuid,
) -> axum::response::Response {
    flash_error_and_redirect(
        headers,
        auth_session
            .user
            .as_ref()
            .and_then(|user| user.preferred_language.clone()),
        messages,
        "relay-not-allowed",
        &format!("/posts/{}", post_id),
    )
}

/// The relay page: the parent's drawing, opened in the painter its home uses.
///
/// The community is what decides that painter -- a two-tone community relays
/// into its own two colours, and its name goes in the bar above the canvas --
/// so a post that has no community relays into the standard painter with
/// nothing named above it. Nothing further along needs one either: `/draw`
/// renders exactly this page for a personal drawing, and `/draw/finish` files
/// what comes back under the parent's community, which for a personal parent is
/// none. That is what makes a personal post relayable at all: the relay stays
/// in the drawer's own namespace instead of landing in a community.
async fn render_relay_page(
    state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    current_user: Option<crate::models::user::User>,
    ftl_lang: String,
    post_id: Uuid,
    post: &std::collections::HashMap<String, Option<String>>,
    community: Option<crate::models::community::Community>,
) -> Result<axum::response::Response, AppError> {
    let template: minijinja::Template<'_, '_> =
        state.env.get_template("draw_post_cucumber.jinja")?;
    let common_ctx = CommonContext::build(tx, current_user.as_ref().map(|user| user.id)).await?;

    let width = post
        .get("image_width")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing image_width".to_string()))?
        .parse::<u32>()?;
    let height = post
        .get("image_height")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing image_height".to_string()))?
        .parse::<u32>()?;
    let image_filename = post
        .get("image_filename")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing image_filename".to_string()))?;

    let painter_mode = match community.as_ref().and_then(|community| {
        Some((
            community.background_color.as_ref()?,
            community.foreground_color.as_ref()?,
        ))
    }) {
        Some((background, foreground)) => json!({
            "kind": "two-tone",
            "backgroundColor": background,
            "foregroundColor": foreground,
        }),
        None => json!({ "kind": "standard" }),
    };
    let painter_config = serde_json::to_string(&json!({
        "width": width,
        "height": height,
        "communityId": community.as_ref().map(|community| community.id.to_string()),
        "parentPostId": post_id.to_string(),
        "initialImageUrl": format!(
            "{}/image/{}/{}?relay={}",
            state.config.r2_public_endpoint_url,
            &image_filename[..2],
            image_filename,
            post_id,
        ),
        "locale": ftl_lang.clone(),
        "submission": { "kind": "post" },
        "mode": painter_mode,
    }))?;

    let rendered = template
        .render(context! {
            parent_post => post,
            current_user => current_user,
            community_name => community.as_ref().map(|community| community.name.clone()),
            width => width,
            height => height,
            background_color => community
                .as_ref()
                .and_then(|community| community.background_color.clone()),
            foreground_color => community
                .as_ref()
                .and_then(|community| community.foreground_color.clone()),
            community_id => community.as_ref().map(|community| community.id.to_string()),
            community_slug => community.as_ref().map(|community| community.slug.clone()),
            is_relay => true,
            draft_post_count => common_ctx.draft_post_count,
            unread_notification_count => common_ctx.unread_notification_count,
            ftl_lang,
            painter_config
        })
        .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;

    Ok(Html(rendered).into_response())
}

pub async fn post_relay_view(
    auth_session: AuthSession,
    headers: HeaderMap,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(id): Path<String>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&id, "/posts", &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };
    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;

    if post.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
        )
            .into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if post is in a private community and if user has access
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    let community = match community_id {
        Some(cid) => find_community_by_id(&mut tx, cid).await?,
        None => None,
    };

    if let Some(ref comm) = community {
        // If community is private, check if user is a member
        if comm.visibility == crate::models::community::CommunityVisibility::Private {
            match &auth_session.user {
                Some(user) => {
                    let user_role = get_user_role_in_community(&mut tx, user.id, comm.id).await?;
                    if user_role.is_none() {
                        // User is not a member of this private community
                        return Ok(flash_error_and_redirect(
                            &headers,
                            user.preferred_language.clone(),
                            messages,
                            "private-community-no-access",
                            "/",
                        ));
                    }
                }
                None => {
                    // Not logged in, cannot access private community - redirect to login
                    return Ok(redirect_to_login(&format!("/posts/{}/relay", id)));
                }
            }
        }
    }
    // Personal posts (community_id is None) are always accessible

    if !relay_is_allowed(&post) {
        return Ok(relay_closed(&headers, &auth_session, messages, uuid));
    }

    render_relay_page(
        &state,
        &mut tx,
        auth_session.user,
        ftl_lang,
        uuid,
        &post,
        community,
    )
    .await
}

pub async fn post_view(
    auth_session: AuthSession,
    headers: HeaderMap,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(id): Path<String>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&id, "/posts", &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };
    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;

    // Store community for later use in template
    let post_community: Option<crate::models::community::Community>;

    match post {
        Some(ref post_data) => {
            // Check if post is in a private community and if user has access
            let community_id = post_data
                .get("community_id")
                .and_then(|v| v.as_ref())
                .and_then(|s| Uuid::parse_str(s).ok());

            let community = if let Some(cid) = community_id {
                find_community_by_id(&mut tx, cid).await?
            } else {
                None
            };
            if let Some(community) = community {
                // If community is private, check if user is a member
                if community.visibility == crate::models::community::CommunityVisibility::Private {
                    match &auth_session.user {
                        Some(user) => {
                            let user_role =
                                get_user_role_in_community(&mut tx, user.id, community.id).await?;
                            if user_role.is_none() {
                                // User is not a member of this private community
                                return Ok(flash_error_and_redirect(
                                    &headers,
                                    user.preferred_language.clone(),
                                    messages,
                                    "private-community-no-access",
                                    "/",
                                ));
                            }
                        }
                        None => {
                            // Not logged in, cannot access private community - redirect to login
                            return Ok(redirect_to_login(&format!("/posts/{}", id)));
                        }
                    }
                }
                post_community = Some(community);
            } else {
                post_community = None;
            }

            increment_post_viewer_count(&mut tx, uuid).await?;
        }
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
            )
                .into_response());
        }
    }

    // At this point post is guaranteed to be Some (would have returned 404 otherwise)
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let comments = build_comment_thread_tree(&mut tx, uuid).await?;

    // Get parent post data if it exists
    let (parent_post_author_login_name, parent_post_data) =
        if let Some(parent_post_id_str) = post.get("parent_post_id").and_then(|id| id.as_ref()) {
            if let Ok(parent_uuid) = Uuid::parse_str(parent_post_id_str) {
                let parent_result = sqlx::query!(
                    r#"
                SELECT
                    posts.id,
                    posts.title,
                    posts.content,
                    posts.author_id,
                    users.login_name AS "login_name?",
                    users.display_name AS "display_name?",
                    actors.handle as "actor_handle?",
                    images.image_filename AS "image_filename?",
                    images.width AS "width?",
                    images.height AS "height?",
                    posts.published_at,
                    COALESCE(comment_counts.count, 0) as comments_count
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                LEFT JOIN actors ON actors.user_id = users.id
                LEFT JOIN (
                    SELECT post_id, COUNT(*) as count
                    FROM comments
                    GROUP BY post_id
                ) comment_counts ON posts.id = comment_counts.post_id
                WHERE posts.id = $1
                AND posts.deleted_at IS NULL
                "#,
                    parent_uuid
                )
                .fetch_optional(&mut *tx)
                .await;

                match parent_result {
                    Ok(Some(row)) => {
                        let login_name = row.login_name.clone().unwrap_or_default();
                        let published_at_formatted = row.published_at.as_ref().map(|dt| {
                            use chrono::TimeZone;
                            let seoul = chrono_tz::Asia::Seoul;
                            let seoul_time = seoul.from_utc_datetime(&dt.naive_utc());
                            seoul_time.format("%Y-%m-%d %H:%M").to_string()
                        });

                        let parent_post = SerializableThreadedPost {
                            id: row.id,
                            title: row.title,
                            content: row.content,
                            author_id: row.author_id,
                            user_login_name: row.login_name.unwrap_or_default(),
                            user_display_name: row.display_name.unwrap_or_default(),
                            user_actor_handle: row.actor_handle.unwrap_or_default(),
                            image_filename: row.image_filename.unwrap_or_default(),
                            image_width: row.width.unwrap_or(0),
                            image_height: row.height.unwrap_or(0),
                            published_at: row.published_at,
                            published_at_formatted,
                            comments_count: row.comments_count.unwrap_or(0),
                            children: Vec::new(),
                        };

                        (login_name, Some(parent_post))
                    }
                    _ => (String::new(), None),
                }
            } else {
                (String::new(), None)
            }
        } else {
            (String::new(), None)
        };

    let community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    // Get collaborative session participants if this post is from a collaborative session
    let collaborative_participants: Vec<CollaborativeParticipant> = sqlx::query!(
        r#"
        SELECT u.login_name, u.display_name
        FROM collaborative_sessions cs
        JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        JOIN users u ON csp.user_id = u.id
        WHERE cs.saved_post_id = $1
        ORDER BY csp.joined_at ASC
        "#,
        uuid
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|row| CollaborativeParticipant {
        login_name: row.login_name,
        display_name: row.display_name,
    })
    .collect();

    // Get reaction counts for this post
    let user_actor_id = if let Some(ref user) = auth_session.user {
        Actor::find_by_user_id(&mut tx, user.id)
            .await
            .ok()
            .flatten()
            .map(|actor| actor.id)
    } else {
        None
    };
    let reaction_counts = get_reaction_counts(&mut tx, uuid, user_actor_id)
        .await
        .unwrap_or_default();

    // Get hashtags for this post
    let hashtags = get_hashtags_for_post(&mut tx, uuid)
        .await
        .unwrap_or_default();

    // Get child posts (threaded replies)
    let child_posts = build_thread_tree(&mut tx, uuid).await.unwrap_or_default();

    tx.commit().await?;

    let community_id = community_id.map(|id| id.to_string());

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_view.jinja")?;

    if headers.get("HX-Request") == Some(&HeaderValue::from_static("true")) {
        let rendered = template
            .eval_to_state(context! {
                current_user => auth_session.user,
                post => Some(&post),
                post_id => id,
                hashtags,
                post_community,
                ftl_lang
            })?
            .render_block("post_edit_block")
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    } else {
        let rendered = template
            .render(context! {
                current_user => auth_session.user,
                        post => Some(&post),
                parent_post_id => post.get("parent_post_id")
                    .and_then(|id| id.as_ref())
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .map(|uuid| uuid.to_string())
                    .unwrap_or_default(),
                parent_post_author_login_name => parent_post_author_login_name.clone(),
                parent_post_data,
                post_id => post.get("id")
                    .and_then(|v| v.as_ref())
                    .ok_or_else(|| AppError::InvalidFormData("Missing post id".to_string()))?
                    .clone(),
                community_id,
                draft_post_count => common_ctx.draft_post_count,
                unread_notification_count => common_ctx.unread_notification_count,
                base_url => state.config.base_url.clone(),
                domain => state.config.domain.clone(),
                comments,
                collaborative_participants,
                reaction_counts,
                hashtags,
                child_posts,
                post_community,
                ftl_lang
            })
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    }
}

/// Whether this viewer may watch the post's replay.
///
/// The recording is kept either way — `allow_replay` says who may watch it, and
/// an author is never shut out of their own. Staff can watch a closed replay
/// too: a drawing gets reported for what happens while it is being drawn as
/// often as for how it ends up, and moderation that cannot see the strokes is
/// moderation of the thumbnail. A post map without the key at all reads as
/// allowed, which is the default the column carries.
fn may_watch_replay(
    post: &std::collections::HashMap<String, Option<String>>,
    viewer: Option<&crate::models::user::User>,
) -> bool {
    let allowed = post
        .get("allow_replay")
        .and_then(|v| v.as_ref())
        .map(|v| v != "false")
        .unwrap_or(true);
    if allowed {
        return true;
    }
    let Some(viewer) = viewer else {
        return false;
    };
    if viewer.is_admin() {
        return true;
    }
    post.get("author_id")
        .and_then(|v| v.as_ref())
        .is_some_and(|author_id| *author_id == viewer.id.to_string())
}

#[cfg(test)]
mod replay_visibility_tests {
    use super::may_watch_replay;
    use crate::models::user::{User, UserRole};
    use std::collections::HashMap;
    use uuid::Uuid;

    fn post(author: Uuid, allow_replay: Option<&str>) -> HashMap<String, Option<String>> {
        let mut post = HashMap::new();
        post.insert("author_id".to_string(), Some(author.to_string()));
        if let Some(allow_replay) = allow_replay {
            post.insert("allow_replay".to_string(), Some(allow_replay.to_string()));
        }
        post
    }

    fn viewer(id: Uuid, role: UserRole) -> User {
        User {
            id,
            login_name: "someone".to_string(),
            password_hash: String::new(),
            display_name: "Someone".to_string(),
            email: None,
            email_verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            banner_id: None,
            preferred_language: None,
            deleted_at: None,
            show_sensitive_content: false,
            role,
        }
    }

    #[test]
    fn an_open_replay_is_open_to_anyone() {
        let author = Uuid::new_v4();
        assert!(may_watch_replay(&post(author, Some("true")), None));
        assert!(may_watch_replay(
            &post(author, Some("true")),
            Some(&viewer(Uuid::new_v4(), UserRole::User))
        ));
    }

    #[test]
    fn a_closed_replay_is_only_the_authors() {
        let author = Uuid::new_v4();
        assert!(may_watch_replay(
            &post(author, Some("false")),
            Some(&viewer(author, UserRole::User))
        ));
        assert!(!may_watch_replay(
            &post(author, Some("false")),
            Some(&viewer(Uuid::new_v4(), UserRole::User))
        ));
        assert!(!may_watch_replay(&post(author, Some("false")), None));
    }

    #[test]
    fn staff_can_watch_a_closed_replay() {
        let author = Uuid::new_v4();
        assert!(may_watch_replay(
            &post(author, Some("false")),
            Some(&viewer(Uuid::new_v4(), UserRole::Admin))
        ));
        // Moderator is not what `is_admin` means, and every other staff gate on
        // the site draws the line in the same place.
        assert!(!may_watch_replay(
            &post(author, Some("false")),
            Some(&viewer(Uuid::new_v4(), UserRole::Moderator))
        ));
    }

    #[test]
    fn a_post_without_the_key_is_open() {
        // The column defaults to true, so its absence must not read as a
        // closed replay -- that would hide every replay on the site.
        assert!(may_watch_replay(&post(Uuid::new_v4(), None), None));
    }
}

pub async fn post_replay_view(
    auth_session: AuthSession,
    headers: HeaderMap,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(id): Path<String>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&id, "/posts", &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };
    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;
    if post.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if post is in a private community and if user has access
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    if let Some(cid) = community_id {
        let community = find_community_by_id(&mut tx, cid).await?;
        if let Some(ref comm) = community {
            // If community is private, check if user is a member
            if comm.visibility == crate::models::community::CommunityVisibility::Private {
                match &auth_session.user {
                    Some(user) => {
                        let user_role =
                            get_user_role_in_community(&mut tx, user.id, comm.id).await?;
                        if user_role.is_none() {
                            // User is not a member of this private community
                            let accept_language = headers
                                .get(axum::http::header::ACCEPT_LANGUAGE)
                                .cloned()
                                .unwrap_or_else(|| axum::http::HeaderValue::from_static(""));
                            let user_preferred_language = user.preferred_language.clone();
                            let bundle = get_bundle(&accept_language, user_preferred_language);
                            let error_message =
                                safe_get_message(&bundle, "private-community-no-access");
                            messages.error(error_message);
                            return Ok(Redirect::to("/").into_response());
                        }
                    }
                    None => {
                        // Not logged in, cannot access private community - redirect to login
                        return Ok(redirect_to_login(&format!("/posts/{}/replay", id)));
                    }
                }
            }
        }
    }
    // Personal posts (community_id is None) are always accessible

    if !may_watch_replay(&post, auth_session.user.as_ref()) {
        // Logged out, this may be the author on another device; otherwise the
        // replay is simply not there to be watched.
        if auth_session.user.is_none() {
            return Ok(redirect_to_login(&format!("/posts/{}/replay", id)));
        }
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let community_id = community_id.map(|id| id.to_string());

    let template_filename = match post.get("replay_filename") {
        Some(replay_filename) => {
            let replay_filename = replay_filename
                .as_ref()
                .ok_or_else(|| AppError::InvalidFormData("Missing replay_filename".to_string()))?;
            if replay_filename.ends_with(".pch") {
                "post_replay_view_pch.jinja"
            } else if replay_filename.ends_with(".tgkr") {
                "post_replay_view_tgkr.jinja"
            } else {
                "post_replay_view_pch.jinja"
            }
        }
        None => "post_replay_view_pch.jinja",
    };

    let template: minijinja::Template<'_, '_> = state.env.get_template(template_filename)?;
    let rendered = template
        .render(context! {
            current_user => auth_session.user,
                post => Some(&post),
            post_id => post.get("id")
                    .and_then(|v| v.as_ref())
                    .ok_or_else(|| AppError::InvalidFormData("Missing post id".to_string()))?
                    .clone(),
            community_id,
            draft_post_count => common_ctx.draft_post_count,
            unread_notification_count => common_ctx.unread_notification_count,
            ftl_lang,
        })
        .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
    Ok(Html(rendered).into_response())
}

pub async fn post_replay_view_mobile(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&id, "/posts", &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };
    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;
    if post.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if post is in a private community and if user has access
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    if let Some(cid) = community_id {
        let community = find_community_by_id(&mut tx, cid).await?;
        if let Some(ref comm) = community {
            // If community is private, check if user is a member
            if comm.visibility == crate::models::community::CommunityVisibility::Private {
                match &auth_session.user {
                    Some(user) => {
                        let user_role =
                            get_user_role_in_community(&mut tx, user.id, comm.id).await?;
                        if user_role.is_none() {
                            // User is not a member of this private community
                            return Ok(StatusCode::FORBIDDEN.into_response());
                        }
                    }
                    None => {
                        // Not logged in, cannot access private community
                        return Ok(StatusCode::FORBIDDEN.into_response());
                    }
                }
            }
        }
    }
    // Personal posts (community_id is None) are always accessible

    if !may_watch_replay(&post, auth_session.user.as_ref()) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let template_filename = match post.get("replay_filename") {
        Some(replay_filename) => {
            let replay_filename = replay_filename
                .as_ref()
                .ok_or_else(|| AppError::InvalidFormData("Missing replay_filename".to_string()))?;
            if replay_filename.ends_with(".pch") {
                "post_replay_view_pch_mobile.jinja"
            } else if replay_filename.ends_with(".tgkr") {
                "post_replay_view_tgkr_mobile.jinja"
            } else {
                "post_replay_view_pch_mobile.jinja"
            }
        }
        None => "post_replay_view_pch_mobile.jinja",
    };

    let template: minijinja::Template<'_, '_> = state.env.get_template(template_filename)?;
    let rendered = template
        .render(context! {
            post => Some(&post),
            r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
            ftl_lang,
        })
        .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
    Ok(Html(rendered).into_response())
}

pub async fn post_publish_form(
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
        return Ok((
            StatusCode::NOT_FOUND,
            handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
        )
            .into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let published_at = post
        .get("published_at")
        .ok_or_else(|| AppError::InvalidFormData("Missing published_at".to_string()))?
        .clone();
    if published_at.is_some() {
        return Ok(Redirect::to(&format!("/posts/{}", id)).into_response());
    }

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    let link = if let Some(cid) = community_id {
        Some(get_community_slug_url(&mut tx, cid).await?)
    } else {
        None
    };

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_form.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        post_id => id,
        link,
        post => {
            post
        },
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct PostPublishForm {
    post_id: String,
    title: String,
    content: String,
    is_sensitive: Option<String>,
    allow_relay: Option<String>,
    allow_replay: Option<String>,
    hashtags: Option<String>,
}

pub async fn post_publish(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Form(form): Form<PostPublishForm>,
) -> Result<impl IntoResponse, AppError> {
    let post_id = Uuid::parse_str(&form.post_id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_id).await?;
    if post.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let author_id = Uuid::parse_str(
        post.get("author_id")
            .and_then(|v| v.as_ref())
            .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?,
    )?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    if author_id != user_id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let is_sensitive = form.is_sensitive == Some("on".to_string());
    let allow_relay = form.allow_relay == Some("on".to_string());
    let allow_replay = form.allow_replay == Some("on".to_string());

    // Parse community_id if present, otherwise None for personal posts
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    // Determine redirect URL based on whether post has community
    let redirect_url = if let Some(cid) = community_id {
        get_community_slug_url(&mut tx, cid).await?
    } else {
        // Personal post - redirect to user's profile
        let user = crate::models::user::find_user_by_id(&mut tx, user_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("User not found"))?;
        format!("/@{}", user.login_name)
    };

    let _ = publish_post(
        &mut tx,
        post_id,
        form.title.clone(),
        form.content.clone(),
        is_sensitive,
        allow_relay,
        allow_replay,
    )
    .await;

    // Not `let _ =`: a tag that fails to store aborts the transaction this
    // publish is running in, so swallowing the error only moved the failure to
    // whichever query ran next and made it unattributable.
    set_post_hashtags(&mut tx, post_id, form.hashtags.as_deref()).await?;

    // Find the actor for this user to send ActivityPub activities
    let actor = Actor::find_by_user_id(&mut tx, user_id).await?;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Check if this is a reply post and notify the parent post author
    if let Some(parent_post_id_str) = post.get("parent_post_id").and_then(|id| id.as_ref()) {
        if let Ok(parent_post_id) = Uuid::parse_str(parent_post_id_str) {
            let parent_post = find_post_by_id(&mut tx, parent_post_id).await?;
            let parent_author_id = parent_post
                .as_ref()
                .and_then(|p| p.get("author_id"))
                .and_then(|id| id.as_ref())
                .and_then(|id| Uuid::parse_str(id).ok());

            if let (Some(actor), Some(parent_author_id)) = (&actor, parent_author_id) {
                // Don't notify if replying to own post
                if parent_author_id != user_id {
                    // For private communities, only notify if parent author is still a member
                    // For personal posts (no community), always notify
                    let should_notify = if let Some(cid) = community_id {
                        let community = find_community_by_id(&mut tx, cid).await?;
                        if let Some(community) = community {
                            if community.visibility
                                == crate::models::community::CommunityVisibility::Private
                            {
                                // Check if parent author is still a member
                                is_user_member(&mut tx, parent_author_id, community.id)
                                    .await
                                    .unwrap_or(false)
                            } else {
                                // Public or unlisted community - always notify
                                true
                            }
                        } else {
                            // No community info - notify anyway
                            true
                        }
                    } else {
                        // Personal post - always notify
                        true
                    };

                    if should_notify {
                        if let Ok(notification) = create_notification(
                            &mut tx,
                            CreateNotificationParams {
                                recipient_id: parent_author_id,
                                actor_id: actor.id,
                                notification_type: NotificationType::PostReply,
                                post_id: Some(post_id),
                                comment_id: None,
                                reaction_iri: None,
                                guestbook_entry_id: None,
                            },
                        )
                        .await
                        {
                            notification_info.push((notification.id, parent_author_id));
                        }
                    }
                }
            }
        }
    }

    // Notify community participants for new posts in unlisted or private communities
    // Only notify for top-level posts (not replies) and only if post has a community
    let is_reply = post
        .get("parent_post_id")
        .and_then(|id| id.as_ref())
        .is_some();

    if let Some(cid) = community_id {
        if !is_reply {
            if let Some(ref actor) = actor {
                let community = find_community_by_id(&mut tx, cid).await?;

                if let Some(ref community) = community {
                    // Only notify for unlisted or private communities
                    let should_notify_community = matches!(
                        community.visibility,
                        crate::models::community::CommunityVisibility::Unlisted
                            | crate::models::community::CommunityVisibility::Private
                    );

                    if should_notify_community {
                        // Get community participants based on visibility
                        let participant_ids: Vec<Uuid> = if community.visibility
                            == crate::models::community::CommunityVisibility::Private
                        {
                            // For private communities, get all members
                            use crate::models::community::get_community_members;
                            let members = get_community_members(&mut tx, cid).await?;
                            members.into_iter().map(|m| m.user_id).collect()
                        } else {
                            // For unlisted communities, get all users who have posted
                            let participants = sqlx::query!(
                                r#"
                                SELECT DISTINCT author_id
                                FROM posts
                                WHERE community_id = $1
                                    AND published_at IS NOT NULL
                                    AND deleted_at IS NULL
                                    AND author_id != $2
                                "#,
                                cid,
                                user_id
                            )
                            .fetch_all(&mut *tx)
                            .await?;
                            participants.into_iter().map(|p| p.author_id).collect()
                        };

                        // Create notifications for each participant (excluding the post author)
                        for participant_id in participant_ids {
                            if participant_id != user_id {
                                if let Ok(notification) = create_notification(
                                    &mut tx,
                                    CreateNotificationParams {
                                        recipient_id: participant_id,
                                        actor_id: actor.id,
                                        notification_type: NotificationType::CommunityPost,
                                        post_id: Some(post_id),
                                        comment_id: None,
                                        reaction_iri: None,
                                        guestbook_entry_id: None,
                                    },
                                )
                                .await
                                {
                                    notification_info.push((notification.id, participant_id));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Determine if we should federate based on post type
    // For personal posts (no community), always federate to user's followers
    // For community posts, only federate if not private
    let (should_federate, community) = if let Some(cid) = community_id {
        let community = find_community_by_id(&mut tx, cid).await?;
        let should_federate = community
            .as_ref()
            .map(|c| c.visibility != crate::models::community::CommunityVisibility::Private)
            .unwrap_or(false);
        (should_federate, community)
    } else {
        // Personal post - always federate to user's followers
        (true, None)
    };

    let _ = tx.commit().await;

    // Send push notifications for created notifications
    if !notification_info.is_empty() {
        let push_service = state.push_service.clone();
        let db_pool = state.db_pool.clone();
        tokio::spawn(async move {
            for (notification_id, recipient_id) in notification_info {
                let mut tx = match db_pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to begin transaction for push notification: {:?}",
                            e
                        );
                        continue;
                    }
                };

                // Get the full notification with actor details
                if let Ok(Some(notification)) =
                    get_notification_by_id(&mut tx, notification_id, recipient_id).await
                {
                    // Get unread count for badge
                    let badge_count = get_unread_count(&mut tx, recipient_id)
                        .await
                        .ok()
                        .and_then(|count| u32::try_from(count).ok());

                    send_push_for_notification(&push_service, &db_pool, &notification, badge_count)
                        .await;
                }
                let _ = tx.commit().await;
            }
        });
    }

    // Send ActivityPub Create activity to followers if actor exists
    // For personal posts: send to user's followers only
    // For community posts: send to both user's and community's followers (if not private)
    if let Some(actor) = actor {
        if should_federate {
            // Send to user's followers first and get the Note object
            match send_post_to_followers(
                &actor,
                post_id,
                form.title.clone(),
                form.content.clone(),
                &state,
            )
            .await
            {
                Ok(note) => {
                    // For community posts, also send to community's followers
                    if let Some(cid) = community_id {
                        if let Err(e) =
                            send_post_to_community_followers(&actor, cid, &note, &state).await
                        {
                            tracing::error!(
                                "Failed to send post to community's ActivityPub followers: {:?}",
                                e
                            );
                            // Don't fail the entire operation if ActivityPub sending fails
                        }
                    } else {
                        tracing::info!("Personal post - sent to user's followers only");
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to send post to user's ActivityPub followers: {:?}",
                        e
                    );
                    // Don't fail the entire operation if ActivityPub sending fails
                }
            }
        } else {
            tracing::info!(
                "Skipping ActivityPub federation for private community post (visibility: {:?})",
                community.as_ref().map(|c| &c.visibility)
            );
        }
    } else {
        tracing::warn!("No actor found for user {}, skipping ActivityPub", user_id);
    }

    Ok(Redirect::to(&redirect_url).into_response())
}

pub async fn draft_posts(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let posts = find_draft_posts_by_author_id(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
    )
    .await?;

    tx.commit().await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("draft_posts.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        posts => posts,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
    })?;

    Ok(Html(rendered))
}

#[derive(Serialize)]
pub struct DraftPostJson {
    pub id: Uuid,
    pub title: Option<String>,
    pub image_url: String,
    pub created_at: String,
    pub community_id: Option<Uuid>,
    pub width: i32,
    pub height: i32,
}

#[derive(Serialize)]
pub struct DraftPostsResponse {
    pub drafts: Vec<DraftPostJson>,
}

pub async fn draft_posts_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<DraftPostsResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let posts = find_draft_posts_by_author_id(&mut tx, user.id).await?;

    tx.commit().await?;

    let drafts: Vec<DraftPostJson> = posts
        .into_iter()
        .map(|post| {
            let image_prefix = &post.image_filename[..2];
            DraftPostJson {
                id: post.id,
                title: post.title,
                image_url: format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, post.image_filename
                ),
                created_at: post.updated_at.to_rfc3339(),
                community_id: post.community_id,
                width: post.image_width,
                height: post.image_height,
            }
        })
        .collect();

    Ok(Json(DraftPostsResponse { drafts }))
}

#[derive(Deserialize)]
pub struct CreateCommentForm {
    pub post_id: String,
    pub parent_comment_id: Option<String>,
    pub content: String,
}

#[derive(Serialize)]
pub struct CollaborativeParticipant {
    pub login_name: String,
    pub display_name: String,
}

pub async fn do_create_comment(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Form(form): Form<CreateCommentForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let post_id =
        Uuid::parse_str(&form.post_id).map_err(|e| AppError::InvalidUuid(format!("{}", e)))?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Get the post to find the author and check access
    let post = find_post_by_id(&mut tx, post_id).await?;

    let post_community = if let Some(ref post_data) = post {
        // Check if post is in a private/unlisted community and if user has access
        let community_id = post_data
            .get("community_id")
            .and_then(|v| v.as_ref())
            .and_then(|s| Uuid::parse_str(s).ok());

        if let Some(cid) = community_id {
            let community = find_community_by_id(&mut tx, cid).await?;
            if let Some(ref comm) = community {
                // If community is private, check if user is a member
                if comm.visibility == crate::models::community::CommunityVisibility::Private {
                    let user_role = get_user_role_in_community(&mut tx, user_id, comm.id).await?;
                    if user_role.is_none() {
                        // User is not a member of this private community
                        return Ok(StatusCode::FORBIDDEN.into_response());
                    }
                }
            }
            community
        } else {
            // Personal post - no community
            None
        }
    } else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };

    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    // Parse parent_comment_id if provided
    let parent_comment_id = form
        .parent_comment_id
        .as_ref()
        .and_then(|id| Uuid::parse_str(id).ok());

    let comment = create_comment(
        &mut tx,
        CommentDraft {
            actor_id: actor.id,
            post_id,
            parent_comment_id,
            content: form.content,
            content_html: None,
        },
    )
    .await?;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // If this is a reply to another comment, notify the parent comment author
    if let Some(parent_id) = parent_comment_id {
        // Fetch the parent comment to get its author
        let parent_comment = sqlx::query!(
            r#"
            SELECT actor_id
            FROM comments
            WHERE id = $1
            "#,
            parent_id
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(parent) = parent_comment {
            // Get the user_id from the parent comment's actor
            let parent_actor = sqlx::query!(
                r#"
                SELECT user_id
                FROM actors
                WHERE id = $1
                "#,
                parent.actor_id
            )
            .fetch_optional(&mut *tx)
            .await?;

            // Only notify if the parent comment author is a local user and not the same as current user
            if let Some(parent_actor_data) = parent_actor {
                if let Some(parent_user_id) = parent_actor_data.user_id {
                    if parent_user_id != user_id {
                        if let Ok(notification) = create_notification(
                            &mut tx,
                            CreateNotificationParams {
                                recipient_id: parent_user_id,
                                actor_id: actor.id,
                                notification_type: NotificationType::CommentReply,
                                post_id: Some(post_id),
                                comment_id: Some(comment.id),
                                reaction_iri: None,
                                guestbook_entry_id: None,
                            },
                        )
                        .await
                        {
                            notification_info.push((notification.id, parent_user_id));
                        }
                    }
                }
            }
        }
    } else {
        // Create notification for the post author (don't notify if commenting on own post)
        // Only send this if it's a top-level comment (no parent)
        if let Some(post_author_id) = post_author_id {
            if post_author_id != user_id {
                if let Ok(notification) = create_notification(
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
                    notification_info.push((notification.id, post_author_id));
                }
            }
        }
    }

    // Extract @mentions from comment content and create notifications
    let mentioned_login_names = extract_mentions(comment.content.as_deref().unwrap_or(""));
    if !mentioned_login_names.is_empty() {
        let mentioned_users = find_users_by_login_names(&mut tx, &mentioned_login_names).await?;
        for (mentioned_user_id, _login_name) in mentioned_users {
            // Don't notify the commenter themselves
            if mentioned_user_id != user_id {
                // For private communities, only notify if mentioned user is a member
                let should_notify = if let Some(ref community) = post_community {
                    if community.visibility
                        == crate::models::community::CommunityVisibility::Private
                    {
                        // Check if mentioned user is a member
                        is_user_member(&mut tx, mentioned_user_id, community.id)
                            .await
                            .unwrap_or(false)
                    } else {
                        // Public or unlisted community - always notify
                        true
                    }
                } else {
                    // No community info - notify anyway
                    true
                };

                if should_notify {
                    if let Ok(notification) = create_notification(
                        &mut tx,
                        CreateNotificationParams {
                            recipient_id: mentioned_user_id,
                            actor_id: actor.id,
                            notification_type: NotificationType::Mention,
                            post_id: Some(post_id),
                            comment_id: Some(comment.id),
                            reaction_iri: None,
                            guestbook_entry_id: None,
                        },
                    )
                    .await
                    {
                        notification_info.push((notification.id, mentioned_user_id));
                    }
                }
            }
        }
    }

    // At this point post is guaranteed to be Some (would have returned 404 otherwise)
    // post is not used after this point, no need to unwrap

    let comments = build_comment_thread_tree(&mut tx, post_id).await?;
    let _ = tx.commit().await;

    // Send push notifications for created notifications
    if !notification_info.is_empty() {
        let push_service = state.push_service.clone();
        let db_pool = state.db_pool.clone();
        tokio::spawn(async move {
            for (notification_id, recipient_id) in notification_info {
                let mut tx = match db_pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to begin transaction for push notification: {:?}",
                            e
                        );
                        continue;
                    }
                };

                if let Ok(Some(notification)) =
                    get_notification_by_id(&mut tx, notification_id, recipient_id).await
                {
                    // Get unread count for badge
                    let badge_count = get_unread_count(&mut tx, recipient_id)
                        .await
                        .ok()
                        .and_then(|count| u32::try_from(count).ok());

                    send_push_for_notification(&push_service, &db_pool, &notification, badge_count)
                        .await;
                }
                let _ = tx.commit().await;
            }
        });
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_comments.jinja")?;
    let rendered = template.render(context! {
        comments => comments,
        current_user => auth_session.user,
        ftl_lang
    })?;
    Ok(Html(rendered).into_response())
}

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
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Check if post is movable using the helper function
    use crate::models::post::is_post_movable;
    if !is_post_movable(&mut tx, post_uuid).await? {
        // Post cannot be moved (part of thread, in private/two-tone community)
        return Ok(StatusCode::FORBIDDEN.into_response());
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

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("post_edit_community.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        post,
        post_id => id,
        current_community,
        unlisted_communities,
        public_participated_communities,
        public_other_communities,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        base_url => state.config.base_url.clone(),
        ftl_lang
    })?;

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
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Check if post is movable using the helper function
    use crate::models::post::is_post_movable;
    if !is_post_movable(&mut tx, post_uuid).await? {
        // Post cannot be moved (part of thread, in private/two-tone community)
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let _ = edit_post_community(&mut tx, post_uuid, form.community_id).await;
    let _ = tx.commit().await;

    Ok(Redirect::to(&format!("/posts/{}", id)).into_response())
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
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Get existing hashtags for this post
    let hashtags = get_hashtags_for_post(&mut tx, post_uuid)
        .await
        .unwrap_or_default();
    let hashtags_string = hashtags
        .iter()
        .map(|h| h.display_name.clone())
        .collect::<Vec<_>>()
        .join(", ");

    tx.commit().await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_edit.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        post,
        post_id => id,
        hashtags => hashtags_string,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct EditPostForm {
    pub title: String,
    pub content: String,
    pub is_sensitive: Option<String>,
    pub allow_relay: Option<String>,
    pub allow_replay: Option<String>,
    pub hashtags: Option<String>,
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
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
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

    set_post_hashtags(&mut tx, post_uuid, form.hashtags.as_deref()).await?;

    let post = find_post_by_id(&mut tx, post_uuid).await?;

    // Get hashtags for this post
    let hashtags = get_hashtags_for_post(&mut tx, post_uuid)
        .await
        .unwrap_or_default();

    // Find the actor for this user to send ActivityPub activities
    let actor = Actor::find_by_user_id(
        &mut tx,
        auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id,
    )
    .await?;

    // Check community visibility before federating updates
    let should_federate = if let Some(ref post_data) = post {
        if let Some(community_id_str) = post_data.get("community_id").and_then(|v| v.as_ref()) {
            if let Ok(community_id) = Uuid::parse_str(community_id_str) {
                let community = find_community_by_id(&mut tx, community_id).await?;
                community
                    .as_ref()
                    .map(|c| c.visibility != crate::models::community::CommunityVisibility::Private)
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
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

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_view.jinja")?;
    let rendered = template
        .eval_to_state(context! {
            current_user => auth_session.user,
                post,
            post_id => id,
            hashtags,
            ftl_lang
        })?
        .render_block("post_edit_block")?;

    Ok(Html(rendered).into_response())
}

pub async fn hx_delete_post(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let post_uuid = Uuid::parse_str(&id)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, post_uuid).await?;
    if post.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if *post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?
        != auth_session
            .user
            .as_ref()
            .ok_or(AppError::Unauthorized)?
            .id
            .to_string()
    {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let image_id = post
        .get("image_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing image_id".to_string()))
        .and_then(|id_str| {
            Uuid::parse_str(id_str).map_err(|e| AppError::InvalidUuid(format!("{}: {}", id_str, e)))
        })?;
    let image = find_image_by_id(&mut tx, image_id).await?;

    let mut keys = vec![format!(
        "image/{}{}/{}",
        image
            .image_filename
            .chars()
            .next()
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        image
            .image_filename
            .chars()
            .nth(1)
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        image.image_filename
    )];

    // Only add replay file to deletion if it exists
    if let Some(ref replay_filename) = image.replay_filename {
        keys.push(format!(
            "replay/{}{}/{}",
            replay_filename
                .chars()
                .next()
                .ok_or_else(|| AppError::InvalidFormData(
                    "Replay filename too short".to_string()
                ))?,
            replay_filename
                .chars()
                .nth(1)
                .ok_or_else(|| AppError::InvalidFormData(
                    "Replay filename too short".to_string()
                ))?,
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
    let redirect_url =
        if let Some(community_id_str) = post_data.get("community_id").and_then(|id| id.clone()) {
            let community_id = Uuid::parse_str(&community_id_str)?;
            get_community_slug_url(&mut tx, community_id).await?
        } else {
            // For personal posts, redirect to user's profile
            let author_id = post_data
                .get("author_id")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| AppError::InvalidFormData("Missing author_id".to_string()))?;
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
    delete_post_with_activity(&mut tx, post_uuid, Some(&state)).await?;
    tx.commit().await?;

    Ok(([("HX-Redirect", &redirect_url)],).into_response())
}

pub async fn post_view_by_login_name(
    auth_session: AuthSession,
    headers: HeaderMap,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path((login_name, post_id)): Path<(String, String)>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&post_id, &format!("/@{}", login_name), &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };

    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;

    // Store community for later use in template
    let post_community: Option<crate::models::community::Community>;

    match post {
        Some(ref post_data) => {
            let post_login_name = post_data
                .get("login_name")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| AppError::InvalidFormData("Missing login_name".to_string()))?;

            // Check if post is in a private community and if user has access
            let community_id = post_data
                .get("community_id")
                .and_then(|v| v.as_ref())
                .and_then(|s| Uuid::parse_str(s).ok());

            let community = if let Some(cid) = community_id {
                find_community_by_id(&mut tx, cid).await?
            } else {
                None
            };

            // A post's page is its author's, whatever community it was posted
            // to: `post_page_path` is where that is decided, and every other
            // handle redirects here.
            if &login_name != post_login_name {
                return Ok(Redirect::to(&crate::models::post::post_page_path(
                    post_login_name,
                    uuid,
                ))
                .into_response());
            }
            if let Some(community) = community {
                // If community is private, check if user is a member
                if community.visibility == crate::models::community::CommunityVisibility::Private {
                    match &auth_session.user {
                        Some(user) => {
                            let user_role =
                                get_user_role_in_community(&mut tx, user.id, community.id).await?;
                            if user_role.is_none() {
                                // User is not a member of this private community
                                return Ok(flash_error_and_redirect(
                                    &headers,
                                    user.preferred_language.clone(),
                                    messages,
                                    "private-community-no-access",
                                    "/",
                                ));
                            }
                        }
                        None => {
                            // Not logged in, cannot access private community - redirect to login
                            return Ok(redirect_to_login(&format!("/@{}/{}", login_name, post_id)));
                        }
                    }
                }
                post_community = Some(community);
            } else {
                post_community = None;
            }

            increment_post_viewer_count(&mut tx, uuid).await?;
        }
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
            )
                .into_response());
        }
    }

    // At this point post is guaranteed to be Some (would have returned 404 otherwise)
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let comments = build_comment_thread_tree(&mut tx, uuid).await?;

    // Get parent post data if it exists
    let (parent_post_author_login_name, parent_post_data) =
        if let Some(parent_post_id_str) = post.get("parent_post_id").and_then(|id| id.as_ref()) {
            if let Ok(parent_uuid) = Uuid::parse_str(parent_post_id_str) {
                // Fetch full parent post data
                let parent_result = sqlx::query!(
                    r#"
                SELECT
                    posts.id,
                    posts.title,
                    posts.content,
                    posts.author_id,
                    users.login_name AS "login_name?",
                    users.display_name AS "display_name?",
                    actors.handle as "actor_handle?",
                    images.image_filename AS "image_filename?",
                    images.width AS "width?",
                    images.height AS "height?",
                    posts.published_at,
                    COALESCE(comment_counts.count, 0) as comments_count
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                LEFT JOIN actors ON actors.user_id = users.id
                LEFT JOIN (
                    SELECT post_id, COUNT(*) as count
                    FROM comments
                    GROUP BY post_id
                ) comment_counts ON posts.id = comment_counts.post_id
                WHERE posts.id = $1
                AND posts.deleted_at IS NULL
                "#,
                    parent_uuid
                )
                .fetch_optional(&mut *tx)
                .await;

                match parent_result {
                    Ok(Some(row)) => {
                        let login_name = row.login_name.clone().unwrap_or_default();

                        // Format the published_at date
                        let published_at_formatted = row.published_at.as_ref().map(|dt| {
                            use chrono::TimeZone;
                            let seoul = chrono_tz::Asia::Seoul;
                            let seoul_time = seoul.from_utc_datetime(&dt.naive_utc());
                            seoul_time.format("%Y-%m-%d %H:%M").to_string()
                        });

                        let parent_post = SerializableThreadedPost {
                            id: row.id,
                            title: row.title,
                            content: row.content,
                            author_id: row.author_id,
                            user_login_name: row.login_name.unwrap_or_default(),
                            user_display_name: row.display_name.unwrap_or_default(),
                            user_actor_handle: row.actor_handle.unwrap_or_default(),
                            image_filename: row.image_filename.unwrap_or_default(),
                            image_width: row.width.unwrap_or(0),
                            image_height: row.height.unwrap_or(0),
                            published_at: row.published_at,
                            published_at_formatted,
                            comments_count: row.comments_count.unwrap_or(0),
                            children: Vec::new(),
                        };

                        (login_name, Some(parent_post))
                    }
                    _ => (String::new(), None),
                }
            } else {
                (String::new(), None)
            }
        } else {
            (String::new(), None)
        };

    let community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    // Get collaborative session participants if this post is from a collaborative session
    let collaborative_participants: Vec<CollaborativeParticipant> = sqlx::query!(
        r#"
        SELECT u.login_name, u.display_name
        FROM collaborative_sessions cs
        JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        JOIN users u ON csp.user_id = u.id
        WHERE cs.saved_post_id = $1
        ORDER BY csp.joined_at ASC
        "#,
        uuid
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|row| CollaborativeParticipant {
        login_name: row.login_name,
        display_name: row.display_name,
    })
    .collect();

    // Get reaction counts for this post
    let user_actor_id = if let Some(ref user) = auth_session.user {
        Actor::find_by_user_id(&mut tx, user.id)
            .await
            .ok()
            .flatten()
            .map(|actor| actor.id)
    } else {
        None
    };
    let reaction_counts = get_reaction_counts(&mut tx, uuid, user_actor_id)
        .await
        .unwrap_or_default();

    // Get hashtags for this post
    let hashtags = get_hashtags_for_post(&mut tx, uuid)
        .await
        .unwrap_or_default();

    // Get child posts (threaded replies)
    let child_posts = build_thread_tree(&mut tx, uuid).await.unwrap_or_default();

    tx.commit().await?;

    let community_id = community_id.map(|id| id.to_string());

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_view.jinja")?;

    if headers.get("HX-Request") == Some(&HeaderValue::from_static("true")) {
        let rendered = template
            .eval_to_state(context! {
                current_user => auth_session.user,
                post => Some(&post),
                post_id => post_id,
                hashtags,
                post_community,
                ftl_lang
            })?
            .render_block("post_edit_block")
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    } else {
        let rendered = template
            .render(context! {
                current_user => auth_session.user,
                        post => Some(&post),
                parent_post_id => post.get("parent_post_id")
                    .and_then(|id| id.as_ref())
                    .and_then(|id| Uuid::parse_str(id).ok())
                    .map(|uuid| uuid.to_string())
                    .unwrap_or_default(),
                parent_post_author_login_name => parent_post_author_login_name.clone(),
                parent_post_data,
                post_id => post.get("id")
                    .and_then(|v| v.as_ref())
                    .ok_or_else(|| AppError::InvalidFormData("Missing post id".to_string()))?
                    .clone(),
                community_id,
                draft_post_count => common_ctx.draft_post_count,
                unread_notification_count => common_ctx.unread_notification_count,
                base_url => state.config.base_url.clone(),
                domain => state.config.domain.clone(),
                comments,
                collaborative_participants,
                reaction_counts,
                hashtags,
                child_posts,
                post_community,
                ftl_lang
            })
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    }
}

pub async fn redirect_post_to_login_name(
    State(state): State<AppState>,
    Path(post_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&post_id, "/posts", &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };

    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;
    tx.commit().await?;

    match post {
        Some(post_data) => {
            let login_name = post_data
                .get("login_name")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| AppError::InvalidFormData("Missing login_name".to_string()))?;
            let post_uuid_str = post_data
                .get("id")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| AppError::InvalidFormData("Missing id".to_string()))?;
            Ok(Redirect::permanent(&format!("/@{}/{}", login_name, post_uuid_str)).into_response())
        }
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

async fn send_post_update_to_followers(
    actor: &Actor,
    post_id: Uuid,
    state: &AppState,
) -> Result<(), AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get all followers for this actor
    let followers = follow::find_followers_by_actor_id(&mut tx, actor.id).await?;

    // Use the function to create the updated Note with timestamp
    let note = create_updated_note_from_post(
        &mut tx,
        post_id,
        actor,
        &state.config.domain,
        &state.config.r2_public_endpoint_url,
    )
    .await?;

    let actor_object_id = ObjectId::parse(&actor.iri)?;
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];
    let published = chrono::Utc::now().to_rfc3339();

    tracing::info!("Updated Note: {:?}", note);

    // Only send to followers if there are any
    if !followers.is_empty() {
        // Create the Update activity for the Note
        let activity_id = generate_object_id(&state.config.domain)?;
        let update = UpdateNote::new(actor_object_id, note, activity_id, to, cc, published);

        // Get follower inboxes
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // Create federation config to send activities
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all followers
            actor
                .send(
                    update,
                    follower_inboxes,
                    state.config.use_activitypub_queue(),
                    &federation_data,
                )
                .await?;
            tracing::info!(
                "Sent Update activity for post {} to {} followers",
                post_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No followers to send Update activity to for post {}",
            post_id
        );
    }

    tx.commit().await?;
    Ok(())
}

pub async fn post_relay_view_by_login_name(
    auth_session: AuthSession,
    headers: HeaderMap,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path((login_name, post_id)): Path<(String, String)>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&post_id, &format!("/@{}", login_name), &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };

    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = match find_post_by_id(&mut tx, uuid).await? {
        Some(post) => post,
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
            )
                .into_response());
        }
    };

    let post_login_name = post
        .get("login_name")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing login_name".to_string()))?;

    // Check if post is in a private community and if user has access
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    let community = match community_id {
        Some(cid) => find_community_by_id(&mut tx, cid).await?,
        None => None,
    };

    // Verify correct slug and redirect if needed
    if let Some(ref comm) = community {
        if login_name != comm.slug {
            let correct_url = format!("/@{}/{}/relay", comm.slug, post_id);
            return Ok(Redirect::to(&correct_url).into_response());
        }
    } else if &login_name != post_login_name {
        let correct_url = format!("/@{}/{}/relay", post_login_name, post_id);
        return Ok(Redirect::to(&correct_url).into_response());
    }

    // Relaying is drawing, and drawing needs an account. This route is outside
    // the login-required group, so without this a signed-out reader is handed
    // the painter and only told at Save that the drawing cannot be kept.
    let Some(user) = auth_session.user.clone() else {
        return Ok(redirect_to_login(&format!(
            "/@{}/{}/relay",
            login_name, post_id
        )));
    };

    if let Some(ref comm) = community {
        // If community is private, check if user is a member
        if comm.visibility == crate::models::community::CommunityVisibility::Private {
            let user_role = get_user_role_in_community(&mut tx, user.id, comm.id).await?;
            if user_role.is_none() {
                // User is not a member of this private community
                return Ok(flash_error_and_redirect(
                    &headers,
                    user.preferred_language.clone(),
                    messages,
                    "private-community-no-access",
                    "/",
                ));
            }
        }
    }

    if !relay_is_allowed(&post) {
        return Ok(relay_closed(&headers, &auth_session, messages, uuid));
    }

    render_relay_page(
        &state,
        &mut tx,
        Some(user),
        ftl_lang,
        uuid,
        &post,
        community,
    )
    .await
}

pub async fn post_replay_view_by_login_name(
    auth_session: AuthSession,
    headers: HeaderMap,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, post_id)): Path<(String, String)>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&post_id, &format!("/@{}", login_name), &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };

    let db = &state.db_pool;
    let mut tx: sqlx::Transaction<'_, sqlx::Postgres> = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;

    match post {
        Some(ref post_data) => {
            let post_login_name = post_data
                .get("login_name")
                .and_then(|v| v.as_ref())
                .ok_or_else(|| AppError::InvalidFormData("Missing login_name".to_string()))?;

            // Check if post is in a private community and if user has access
            let community_id = post_data
                .get("community_id")
                .and_then(|v| v.as_ref())
                .and_then(|s| Uuid::parse_str(s).ok());

            let community = if let Some(cid) = community_id {
                find_community_by_id(&mut tx, cid).await?
            } else {
                None
            };

            // Verify correct slug and redirect if needed
            if let Some(ref comm) = community {
                if login_name != comm.slug {
                    let correct_url = format!("/@{}/{}/replay", comm.slug, post_id);
                    return Ok(Redirect::to(&correct_url).into_response());
                }
            } else if &login_name != post_login_name {
                let correct_url = format!("/@{}/{}/replay", post_login_name, post_id);
                return Ok(Redirect::to(&correct_url).into_response());
            }

            if let Some(ref comm) = community {
                // If community is private, check if user is a member
                if comm.visibility == crate::models::community::CommunityVisibility::Private {
                    match &auth_session.user {
                        Some(user) => {
                            let user_role =
                                get_user_role_in_community(&mut tx, user.id, comm.id).await?;
                            if user_role.is_none() {
                                // User is not a member of this private community
                                return Ok(flash_error_and_redirect(
                                    &headers,
                                    user.preferred_language.clone(),
                                    messages,
                                    "private-community-no-access",
                                    "/",
                                ));
                            }
                        }
                        None => {
                            // Not logged in, cannot access private community - redirect to login
                            return Ok(redirect_to_login(&format!(
                                "/@{}/{}/replay",
                                login_name, post_id
                            )));
                        }
                    }
                }
            }
        }
        None => {
            return Ok(StatusCode::NOT_FOUND.into_response());
        }
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    if !may_watch_replay(&post, auth_session.user.as_ref()) {
        if auth_session.user.is_none() {
            return Ok(redirect_to_login(&format!(
                "/@{}/{}/replay",
                login_name, post_id
            )));
        }
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community_id = post
        .get("community_id")
        .and_then(|id| id.as_ref())
        .and_then(|id_str| Uuid::parse_str(id_str).ok());

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let community_id = community_id.map(|id| id.to_string());

    let template_filename = match post.get("replay_filename") {
        Some(replay_filename) => {
            let replay_filename = replay_filename
                .as_ref()
                .ok_or_else(|| AppError::InvalidFormData("Missing replay_filename".to_string()))?;
            if replay_filename.ends_with(".pch") {
                "post_replay_view_pch.jinja"
            } else if replay_filename.ends_with(".tgkr") {
                "post_replay_view_tgkr.jinja"
            } else {
                "post_replay_view_pch.jinja"
            }
        }
        None => "post_replay_view_pch.jinja",
    };

    let template: minijinja::Template<'_, '_> = state.env.get_template(template_filename)?;
    let rendered = template
        .render(context! {
            current_user => auth_session.user,
            post => Some(&post),
            post_id => post_id,
            community_id,
            draft_post_count => common_ctx.draft_post_count,
            unread_notification_count => common_ctx.unread_notification_count,
            ftl_lang
        })
        .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct AddReactionForm {
    pub emoji: String,
}

pub async fn add_reaction(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    Form(form): Form<AddReactionForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let post_id = Uuid::parse_str(&post_id)?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Get post data and check access
    let post = find_post_by_id(&mut tx, post_id).await?;

    if let Some(ref post_data) = post {
        // Check if post is in a private/unlisted community and if user has access
        let community_id = post_data
            .get("community_id")
            .and_then(|v| v.as_ref())
            .and_then(|s| Uuid::parse_str(s).ok());

        if let Some(cid) = community_id {
            let community = find_community_by_id(&mut tx, cid).await?;
            if let Some(community) = community {
                // If community is private, check if user is a member
                if community.visibility == crate::models::community::CommunityVisibility::Private {
                    let user_role =
                        get_user_role_in_community(&mut tx, user_id, community.id).await?;
                    if user_role.is_none() {
                        // User is not a member of this private community
                        return Ok(StatusCode::FORBIDDEN.into_response());
                    }
                }
            }
        }
        // Personal posts (community_id is None) are always accessible for reactions
    } else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let reaction = create_reaction(
        &mut tx,
        ReactionDraft {
            post_id,
            actor_id: actor.id,
            emoji: form.emoji.clone(),
        },
        &state.config.domain,
    )
    .await?;
    let login_name = post
        .as_ref()
        .and_then(|p| p.get("login_name"))
        .and_then(|l| l.as_ref())
        .unwrap_or(&String::new())
        .clone();

    // Get post author's actor for sending ActivityPub activity
    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the post author (don't notify if reacting to own post)
    if let Some(post_author_id) = post_author_id {
        if post_author_id != user_id {
            if let Ok(notification) = create_notification(
                &mut tx,
                CreateNotificationParams {
                    recipient_id: post_author_id,
                    actor_id: actor.id,
                    notification_type: NotificationType::Reaction,
                    post_id: Some(post_id),
                    comment_id: None,
                    reaction_iri: Some(reaction.iri.clone()),
                    guestbook_entry_id: None,
                },
            )
            .await
            {
                notification_info.push((notification.id, post_author_id));
            }
        }
    }

    let user_actor_id = Some(actor.id);
    let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;
    tx.commit().await?;

    // Send push notifications for created notifications
    if !notification_info.is_empty() {
        let push_service = state.push_service.clone();
        let db_pool = state.db_pool.clone();
        tokio::spawn(async move {
            for (notification_id, recipient_id) in notification_info {
                let mut tx = match db_pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to begin transaction for push notification: {:?}",
                            e
                        );
                        continue;
                    }
                };

                if let Ok(Some(notification)) =
                    get_notification_by_id(&mut tx, notification_id, recipient_id).await
                {
                    // Get unread count for badge
                    let badge_count = get_unread_count(&mut tx, recipient_id)
                        .await
                        .ok()
                        .and_then(|count| u32::try_from(count).ok());

                    send_push_for_notification(&push_service, &db_pool, &notification, badge_count)
                        .await;
                }
                let _ = tx.commit().await;
            }
        });
    }

    // Send EmojiReact activity to post author if they're remote or local with followers
    if let Some(author_id) = post_author_id {
        if author_id != user_id {
            // Only send if reacting to someone else's post
            let mut tx = db.begin().await?;
            let post_author_actor = Actor::find_by_user_id(&mut tx, author_id).await?;
            tx.commit().await?;

            if let Some(post_author_actor) = post_author_actor {
                // Build EmojiReact activity
                use crate::web::handlers::activitypub::EmojiReact;

                let post_url = format!(
                    "https://{}/@{}/{}",
                    state.config.domain, login_name, post_id
                );

                let emoji_react = EmojiReact {
                    actor: Some(activitypub_federation::fetch::object_id::ObjectId::parse(
                        &actor.iri,
                    )?),
                    object: post_url.parse()?,
                    content: form.emoji.clone(),
                    r#type: "EmojiReact".to_string(),
                    id: reaction.iri.parse()?,
                    to: vec![post_author_actor.iri.clone()],
                    cc: vec![],
                    signature: None,
                };

                // Create federation config
                let federation_config = activitypub_federation::config::FederationConfig::builder()
                    .domain(&state.config.domain)
                    .app_data(state.clone())
                    .build()
                    .await?;
                let federation_data = federation_config.to_request_data();

                // Send to post author's inbox
                if let Err(e) = actor
                    .send(
                        emoji_react,
                        vec![post_author_actor.shared_inbox_or_inbox()],
                        state.config.use_activitypub_queue(),
                        &federation_data,
                    )
                    .await
                {
                    tracing::error!("Failed to send EmojiReact activity: {:?}", e);
                    // Don't fail the request if ActivityPub sending fails
                }
            }
        }
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_reactions.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        reaction_counts => reaction_counts,
        post_id => post_id.to_string(),
        login_name => login_name,
    })?;
    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct RemoveReactionForm {
    pub emoji: String,
}

pub async fn remove_reaction(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(post_id): Path<String>,
    Form(form): Form<RemoveReactionForm>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user_id = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?.id;
    let post_id = Uuid::parse_str(&post_id)?;

    // Get the actor for this user
    let actor = Actor::find_by_user_id(&mut tx, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No actor found for user"))?;

    // Get post data and check access
    let post = find_post_by_id(&mut tx, post_id).await?;

    if let Some(ref post_data) = post {
        // Check if post is in a private/unlisted community and if user has access
        let community_id = post_data
            .get("community_id")
            .and_then(|v| v.as_ref())
            .and_then(|s| Uuid::parse_str(s).ok());

        if let Some(cid) = community_id {
            let community = find_community_by_id(&mut tx, cid).await?;
            if let Some(community) = community {
                // If community is private, check if user is a member
                if community.visibility == crate::models::community::CommunityVisibility::Private {
                    let user_role =
                        get_user_role_in_community(&mut tx, user_id, community.id).await?;
                    if user_role.is_none() {
                        // User is not a member of this private community
                        return Ok(StatusCode::FORBIDDEN.into_response());
                    }
                }
            }
        }
        // Personal posts (community_id is None) are always accessible for reactions
    } else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    // Find the reaction before deleting (need IRI for Undo activity)
    use crate::models::reaction::find_user_reaction;
    let existing_reaction = find_user_reaction(&mut tx, post_id, actor.id, &form.emoji).await?;

    let _ = delete_reaction(&mut tx, post_id, actor.id, &form.emoji).await;
    let login_name = post
        .as_ref()
        .and_then(|p| p.get("login_name"))
        .and_then(|l| l.as_ref())
        .unwrap_or(&String::new())
        .clone();

    // Get post author's actor for sending ActivityPub activity
    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    let user_actor_id = Some(actor.id);
    let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;
    tx.commit().await?;

    // Send Undo(EmojiReact) activity to post author
    if let Some(reaction) = existing_reaction {
        if let Some(author_id) = post_author_id {
            if author_id != user_id {
                // Only send if unreacting to someone else's post
                let mut tx = db.begin().await?;
                let post_author_actor = Actor::find_by_user_id(&mut tx, author_id).await?;
                tx.commit().await?;

                if let Some(post_author_actor) = post_author_actor {
                    // Build EmojiReact activity (the object being undone)
                    use crate::web::handlers::activitypub::{
                        generate_object_id, EmojiReact, Undo, UndoObject,
                    };

                    let post_url = format!(
                        "https://{}/@{}/{}",
                        state.config.domain, login_name, post_id
                    );

                    let emoji_react = EmojiReact {
                        actor: Some(activitypub_federation::fetch::object_id::ObjectId::parse(
                            &actor.iri,
                        )?),
                        object: post_url.parse()?,
                        content: form.emoji.clone(),
                        r#type: "EmojiReact".to_string(),
                        id: reaction.iri.parse()?,
                        to: vec![post_author_actor.iri.clone()],
                        cc: vec![],
                        signature: None,
                    };

                    // Build Undo activity
                    let undo_id = generate_object_id(&state.config.domain)?;
                    let undo = Undo {
                        actor: activitypub_federation::fetch::object_id::ObjectId::parse(
                            &actor.iri,
                        )?,
                        object: UndoObject::EmojiReact(Box::new(emoji_react)),
                        r#type: activitystreams_kinds::activity::UndoType::Undo,
                        id: undo_id,
                    };

                    // Create federation config
                    let federation_config =
                        activitypub_federation::config::FederationConfig::builder()
                            .domain(&state.config.domain)
                            .app_data(state.clone())
                            .build()
                            .await?;
                    let federation_data = federation_config.to_request_data();

                    // Send to post author's inbox
                    if let Err(e) = actor
                        .send(
                            undo,
                            vec![post_author_actor.shared_inbox_or_inbox()],
                            state.config.use_activitypub_queue(),
                            &federation_data,
                        )
                        .await
                    {
                        tracing::error!("Failed to send Undo(EmojiReact) activity: {:?}", e);
                        // Don't fail the request if ActivityPub sending fails
                    }
                }
            }
        }
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("post_reactions.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        reaction_counts => reaction_counts,
        post_id => post_id.to_string(),
        login_name => login_name,
    })?;
    Ok(Html(rendered).into_response())
}

pub async fn post_reactions_detail(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, post_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let uuid = match parse_id_with_legacy_support(&post_id, &format!("/@{}", login_name), &state)? {
        ParsedId::Uuid(uuid) => uuid,
        ParsedId::Redirect(redirect) => return Ok(redirect.into_response()),
        ParsedId::InvalidId(error_response) => return Ok(error_response),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let post = find_post_by_id(&mut tx, uuid).await?;

    if post.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            handler_404(auth_session, ExtractFtlLang(ftl_lang), State(state)).await?,
        )
            .into_response());
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let post_data = post.clone();
    let post_login_name = post_data
        .get("login_name")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::InvalidFormData("Missing login_name".to_string()))?;

    // Check if post has community and verify correct slug
    let community_id = post_data
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    if let Some(cid) = community_id {
        let community = find_community_by_id(&mut tx, cid).await?;
        if let Some(comm) = community {
            if login_name != comm.slug {
                let correct_url = format!("/@{}/{}/reactions", comm.slug, post_id);
                return Ok(Redirect::to(&correct_url).into_response());
            }
        }
    } else {
        // No community - verify slug is author login_name
        if &login_name != post_login_name {
            let correct_url = format!("/@{}/{}/reactions", post_login_name, post_id);
            return Ok(Redirect::to(&correct_url).into_response());
        }
    }

    // Get all reactions for this post
    let reactions = find_reactions_by_post_id(&mut tx, uuid).await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    tx.commit().await?;

    // Group reactions by emoji
    use std::collections::HashMap;
    let mut grouped_reactions_map: HashMap<String, Vec<_>> = HashMap::new();
    for reaction in reactions {
        grouped_reactions_map
            .entry(reaction.emoji.clone())
            .or_insert_with(Vec::new)
            .push(reaction);
    }

    // Convert HashMap to Vec for template
    #[derive(Serialize)]
    struct ReactionForTemplate {
        actor_name: String,
        actor_handle: String,
        actor_url: String,
        is_local: bool,
        created_at: String,
    }

    #[derive(Serialize)]
    struct EmojiGroup {
        emoji: String,
        reactions: Vec<ReactionForTemplate>,
    }

    let grouped_reactions: Vec<EmojiGroup> = grouped_reactions_map
        .into_iter()
        .map(|(emoji, reactions)| {
            let reactions_for_template = reactions
                .into_iter()
                .map(|r| {
                    let is_local = r.actor_login_name.is_some();
                    ReactionForTemplate {
                        actor_name: r.actor_name,
                        actor_handle: r.actor_handle,
                        actor_url: r.actor_url,
                        is_local,
                        created_at: r.created_at.to_rfc3339(),
                    }
                })
                .collect();
            EmojiGroup {
                emoji,
                reactions: reactions_for_template,
            }
        })
        .collect();

    let template = state.env.get_template("post_reactions_detail.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        post_title => post_data.get("title").and_then(|t| t.as_ref()).unwrap_or(&"Untitled".to_string()),
        post_id => post_id,
        login_name => login_name,
        grouped_reactions => grouped_reactions,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

/// API endpoint to get list of communities that a post can be moved to
pub async fn get_movable_communities_api(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(post_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    use crate::models::post::{get_movable_communities, is_post_movable};
    use crate::web::responses::post::{MovableCommunitiesResponse, MovableCommunity};

    let user = auth_session.user.ok_or(AppError::Unauthorized)?;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get post data to check ownership and movability
    let post = sqlx::query!(
        r#"SELECT id, author_id, community_id FROM posts WHERE id = $1 AND deleted_at IS NULL"#,
        post_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if user is the author
    if post.author_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Check if post is movable
    if !is_post_movable(&mut tx, post_id).await? {
        // Return empty list if post is not movable
        return Ok(Json(MovableCommunitiesResponse {
            communities: vec![],
        })
        .into_response());
    }

    // Get list of movable communities
    let communities = get_movable_communities(&mut tx, user.id).await?;

    // Build response - only add "Personal Post" option if post is currently in a community
    let mut response_communities = vec![];
    if post.community_id.is_some() {
        response_communities.push(MovableCommunity {
            id: None,
            name: "Personal Post".to_string(),
            slug: None,
            visibility: None,
            background_color: None,
            foreground_color: None,
            owner_login_name: None,
            owner_display_name: None,
            has_participated: None,
        });
    }

    // Add other communities, excluding the current community and private communities
    for community in communities {
        // Skip the current community
        if Some(community.id) == post.community_id {
            continue;
        }

        // Skip private communities - moving to private communities is restricted
        if matches!(
            community.visibility,
            crate::models::community::CommunityVisibility::Private
        ) {
            continue;
        }

        response_communities.push(MovableCommunity {
            id: Some(community.id),
            name: community.name,
            slug: Some(community.slug),
            visibility: Some(community.visibility),
            background_color: community.background_color,
            foreground_color: community.foreground_color,
            owner_login_name: Some(community.owner_login_name),
            owner_display_name: Some(community.owner_display_name),
            has_participated: Some(community.has_participated),
        });
    }

    tx.commit().await?;

    Ok(Json(MovableCommunitiesResponse {
        communities: response_communities,
    })
    .into_response())
}

/// API endpoint to move a post to a different community
pub async fn move_post_community_api(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(post_id): Path<Uuid>,
    Json(payload): Json<crate::web::responses::post::MoveCommunityRequest>,
) -> Result<impl IntoResponse, AppError> {
    use crate::models::community::{find_community_by_id, is_user_member};
    use crate::models::post::is_post_movable;

    let user = auth_session.user.ok_or(AppError::Unauthorized)?;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get post data to check ownership and current community
    let post = sqlx::query!(
        r#"SELECT id, author_id, community_id FROM posts WHERE id = $1 AND deleted_at IS NULL"#,
        post_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    // Check if user is the author
    if post.author_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Check if post is movable
    if !is_post_movable(&mut tx, post_id).await? {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // If moving to a community (not personal), validate access and restrictions
    if let Some(target_community_id) = payload.community_id {
        let target_community = find_community_by_id(&mut tx, target_community_id)
            .await?
            .ok_or_else(|| AppError::NotFound("Community".to_string()))?;

        // Check if target is a two-tone community (both colors set)
        if target_community.background_color.is_some()
            && target_community.foreground_color.is_some()
        {
            return Ok(StatusCode::FORBIDDEN.into_response());
        }

        // Check user has access to target community
        use crate::models::community::CommunityVisibility;
        match target_community.visibility {
            CommunityVisibility::Private => {
                // Must be a member of private communities
                if !is_user_member(&mut tx, target_community_id, user.id).await? {
                    return Ok(StatusCode::FORBIDDEN.into_response());
                }
            }
            CommunityVisibility::Public | CommunityVisibility::Unlisted => {
                // Anyone can post to public/unlisted communities
            }
        }
    }

    // Move the post
    edit_post_community(&mut tx, post_id, payload.community_id).await?;

    tx.commit().await?;

    // Return success with updated post data
    Ok(StatusCode::OK.into_response())
}
