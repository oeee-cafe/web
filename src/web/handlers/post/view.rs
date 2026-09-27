//! A post's pages: the drawing, its relay and its replay.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::comment::build_comment_thread_tree;
use crate::models::community::{find_community_by_id, get_user_role_in_community};
use crate::models::post::{
    build_thread_tree, find_post_by_id, increment_post_viewer_count, SerializableThreadedPost,
};
use crate::models::reaction::get_reaction_counts;
use crate::models::tag::get_tags_for_post;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::handlers::{parse_id_with_legacy_support, ParsedId};
use crate::web::i18n::ExtractFtlLang;
use crate::web::presence::{Activity, Presence};
use crate::web::state::AppState;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html};
use axum_messages::Messages;
use minijinja::context;
use serde_json::json;
use uuid::Uuid;

use super::comment::CollaborativeParticipant;
use super::{flash_error_and_redirect, redirect_to_canonical_post, redirect_to_login};

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
    let template = "draw_post_cucumber.jinja";
    let common_ctx = CommonContext::build(tx, current_user.as_ref(), &ftl_lang).await?;

    let width = post
        .get("image_width")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing image_width".to_string()))?
        .parse::<u32>()?;
    let height = post
        .get("image_height")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing image_height".to_string()))?
        .parse::<u32>()?;
    let image_filename = post
        .get("image_filename")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing image_filename".to_string()))?;

    let mut painter_config = crate::web::handlers::draw::post_painter_config(
        width,
        height,
        community.as_ref(),
        Some(&post_id.to_string()),
        &ftl_lang,
        current_user.as_ref(),
    );
    painter_config["initialImageUrl"] = json!(format!(
        "{}/image/{}/{}?relay={}",
        state.config.r2_public_endpoint_url,
        &image_filename[..2],
        image_filename,
        post_id,
    ));
    painter_config["submission"] = json!({ "kind": "post" });
    let painter_config = serde_json::to_string(&painter_config)?;

    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                parent_post => post,
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
                painter_config
            },
        )
        .await
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
        return Err(AppError::NotFound("Post".to_string()));
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
            return Err(AppError::NotFound("Post".to_string()));
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
                    COALESCE(comment_counts.count, 0) as comments_count,
                    communities.slug AS "community_slug?"
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                LEFT JOIN actors ON actors.user_id = users.id
                LEFT JOIN communities ON posts.community_id = communities.id
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
                            user_login_name: (row.login_name.unwrap_or_default()).into(),
                            user_display_name: row.display_name.unwrap_or_default(),
                            user_actor_handle: row.actor_handle.unwrap_or_default(),
                            image_filename: row.image_filename.unwrap_or_default(),
                            image_width: row.width.unwrap_or(0),
                            image_height: row.height.unwrap_or(0),
                            published_at: row.published_at,
                            published_at_formatted,
                            comments_count: row.comments_count.unwrap_or(0),
                            community_slug: row.community_slug,
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

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

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

    // Get tags for this post
    let tags = get_tags_for_post(&mut tx, uuid).await.unwrap_or_default();

    // Get child posts (threaded replies)
    let child_posts = build_thread_tree(&mut tx, uuid).await.unwrap_or_default();

    tx.commit().await?;

    let community_id = community_id.map(|id| id.to_string());

    let template = "post_view.jinja";

    if headers.get("HX-Request") == Some(&HeaderValue::from_static("true")) {
        let rendered = state
            .render_block(
                template,
                "post_edit_block",
                context! {
                    current_user => auth_session.user,
                    post => Some(&post),
                    post_id => id,
                    tags,
                    post_community,
                    ftl_lang
                },
            )
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    } else {
        let rendered = state
            .render_page(
                template,
                common_ctx,
                context! {
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
                        .ok_or_else(|| AppError::BadRequest("Missing post id".to_string()))?
                        .clone(),
                    community_id,
                    base_url => state.config.base_url.clone(),
                    domain => state.config.domain.clone(),
                    comments,
                    collaborative_participants,
                    reaction_counts,
                    tags,
                    child_posts,
                    post_community,
                },
            )
            .await
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
            login_name: "someone".into(),
            password_hash: None,
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
                .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;

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

            // `post_page_path` decides where a post's page is, and every other
            // handle redirects there.
            if let Some(redirect) = redirect_to_canonical_post(
                &login_name,
                post_login_name,
                community.as_ref(),
                uuid,
                "",
            ) {
                return Ok(redirect);
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
            return Err(AppError::NotFound("Post".to_string()));
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
                    COALESCE(comment_counts.count, 0) as comments_count,
                    communities.slug AS "community_slug?"
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                LEFT JOIN actors ON actors.user_id = users.id
                LEFT JOIN communities ON posts.community_id = communities.id
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
                            user_login_name: (row.login_name.unwrap_or_default()).into(),
                            user_display_name: row.display_name.unwrap_or_default(),
                            user_actor_handle: row.actor_handle.unwrap_or_default(),
                            image_filename: row.image_filename.unwrap_or_default(),
                            image_width: row.width.unwrap_or(0),
                            image_height: row.height.unwrap_or(0),
                            published_at: row.published_at,
                            published_at_formatted,
                            comments_count: row.comments_count.unwrap_or(0),
                            community_slug: row.community_slug,
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

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

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

    // Get tags for this post
    let tags = get_tags_for_post(&mut tx, uuid).await.unwrap_or_default();

    // Get child posts (threaded replies)
    let child_posts = build_thread_tree(&mut tx, uuid).await.unwrap_or_default();

    tx.commit().await?;

    let community_id = community_id.map(|id| id.to_string());

    let template = "post_view.jinja";

    if headers.get("HX-Request") == Some(&HeaderValue::from_static("true")) {
        let rendered = state
            .render_block(
                template,
                "post_edit_block",
                context! {
                    current_user => auth_session.user,
                    post => Some(&post),
                    post_id => post_id,
                    tags,
                    post_community,
                    ftl_lang
                },
            )
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
        Ok(Html(rendered).into_response())
    } else {
        let rendered = state
            .render_page(
                template,
                common_ctx,
                context! {
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
                        .ok_or_else(|| AppError::BadRequest("Missing post id".to_string()))?
                        .clone(),
                    community_id,
                    base_url => state.config.base_url.clone(),
                    domain => state.config.domain.clone(),
                    comments,
                    collaborative_participants,
                    reaction_counts,
                    tags,
                    child_posts,
                    post_community,
                },
            )
            .await
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
                .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;
            let community_slug = post_data.get("community_slug").and_then(|v| v.as_deref());
            // Temporary, not permanent: the page moves when the post moves
            // between communities, and a cached 308 would outlive that.
            Ok(Redirect::to(&crate::models::post::post_page_path(
                login_name,
                community_slug,
                uuid,
            ))
            .into_response())
        }
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
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
            return Err(AppError::NotFound("Post".to_string()));
        }
    };

    let post_login_name = post
        .get("login_name")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;

    // Check if post is in a private community and if user has access
    let community_id = post
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok());

    let community = match community_id {
        Some(cid) => find_community_by_id(&mut tx, cid).await?,
        None => None,
    };

    if let Some(redirect) = redirect_to_canonical_post(
        &login_name,
        post_login_name,
        community.as_ref(),
        uuid,
        "/relay",
    ) {
        return Ok(redirect);
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
                .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;

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

            if let Some(redirect) = redirect_to_canonical_post(
                &login_name,
                post_login_name,
                community.as_ref(),
                uuid,
                "/replay",
            ) {
                return Ok(redirect);
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

    // Only Tegaki's recordings have a page of their own: its player takes
    // the whole window. A NEO replay plays on the post page, in place of the
    // drawing, and this address is never linked for one, so an old link to
    // it finds nothing.
    let is_tegaki = post
        .get("replay_filename")
        .and_then(|name| name.as_deref())
        .is_some_and(|name| name.ends_with(".tgkr"));
    if !is_tegaki {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let community_id = community_id.map(|id| id.to_string());

    let template = "post_replay_view_tgkr.jinja";
    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                presence => Presence::new(Activity::WatchingReplay),
                post => Some(&post),
                post_id => post_id,
                community_id,
            },
        )
        .await
        .map_err(|e| AppError::from(anyhow::anyhow!("Template render error: {}", e)))?;
    Ok(Html(rendered).into_response())
}
