use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::handlers::{ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use minijinja::context;
use uuid::Uuid;

use super::db;
use super::preview::preview_versions;
use super::types::*;
use super::utils::get_preferred_locale;

/// The canvases a session can be created with.
///
/// One list, because the lobby form offers these and the create endpoint has to
/// insist on them. It used to only offer them: `width` and `height` went from
/// the request body into the row untouched, so a session could be created at
/// any size at all -- and a session's size is what bounds every checkpoint
/// snapshot of it, which is what `MAX_SNAPSHOT_BYTES` is derived from and what
/// the history ceiling is sized against.
pub const CANVAS_SIZES: [(u32, u32); 2] = [(300, 300), (1024, 768)];

/// The seats a session can offer. A checkpoint holds two snapshots per
/// participant, so this is the other half of what bounds one.
pub const MAX_PARTICIPANTS_CHOICES: [i32; 4] = [2, 4, 6, 8];

/// `CANVAS_SIZES` as the form wants them: the value it posts back, and the
/// label a person reads.
fn canvas_size_options() -> Vec<(String, String)> {
    CANVAS_SIZES
        .iter()
        .map(|(width, height)| (format!("{width}x{height}"), format!("{width}×{height}")))
        .collect()
}

pub async fn get_auth_info(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
) -> impl IntoResponse {
    match auth_session.user {
        Some(user) => {
            let preferred_locale =
                get_preferred_locale(user.preferred_language.clone(), &accept_language);

            (
                StatusCode::OK,
                Json(AuthInfo {
                    user_id: user.id.to_string(),
                    login_name: user.login_name,
                    preferred_locale,
                }),
            )
                .into_response()
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Authentication required"
            })),
        )
            .into_response(),
    }
}

pub async fn get_collaboration_meta(
    Path(session_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<CollaborationMeta>, AppError> {
    let _user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("Authentication required"))?;

    let db = &state.db_pool;

    let session = sqlx::query!(
        r#"
        SELECT cs.title, cs.width, cs.height, cs.owner_id, cs.saved_post_id, cs.max_participants, u.login_name as owner_login_name
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        WHERE cs.id = $1 AND cs.ended_at IS NULL
        "#,
        session_uuid
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Session not found or not active"))?;

    let user_count = sqlx::query_scalar!(
        r#"
        SELECT COUNT(DISTINCT user_id) as "count!"
        FROM collaborative_sessions_participants
        WHERE session_id = $1 AND is_active = true
        "#,
        session_uuid
    )
    .fetch_one(db)
    .await?;

    Ok(Json(CollaborationMeta {
        title: session
            .title
            .unwrap_or_else(|| "Untitled Collaboration".to_string()),
        width: session.width,
        height: session.height,
        owner_id: session.owner_id.to_string(),
        saved_post_id: session.saved_post_id.map(|id| id.to_string()),
        owner_login_name: session.owner_login_name,
        max_users: session.max_participants,
        current_user_count: user_count,
    }))
}

#[derive(serde::Deserialize)]
pub struct LobbyQuery {
    /// Slug to preselect, so a community page can link straight into the
    /// create form with its own community chosen.
    pub community: Option<String>,
}

/// How many lobby cards either session list shows.
const SESSION_LIST_LIMIT: i64 = 20;

/// Every open public session, annotated for the viewer.
///
/// Full sessions stay in the list. Hiding them made a busy canvas look like it
/// had ended, and the one case where a full session is still joinable — the
/// viewer is already a participant — is exactly the case that vanished.
async fn find_public_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    viewer_user_id: Option<Uuid>,
) -> Result<Vec<SessionWithCounts>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            cs.id,
            u.login_name AS owner_login_name,
            cs.title,
            cs.width,
            cs.height,
            cs.created_at,
            cs.max_participants,
            cs.is_public,
            c.name AS "community_name?",
            c.slug AS "community_slug?",
            COALESCE(COUNT(DISTINCT csp.user_id) FILTER (WHERE csp.is_active = true), 0) AS "participant_count!",
            -- COALESCE, not a bare comparison: $1 is NULL for a signed-out
            -- visitor, and NULL decoded into a non-null bool fails the query.
            COALESCE(cs.owner_id = $1, false) AS "is_owner!",
            COUNT(*) FILTER (WHERE csp.user_id = $1 AND csp.is_active = true) > 0 AS "viewer_is_participant!"
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        WHERE cs.is_public = true
          AND cs.ended_at IS NULL
          AND (cs.community_id IS NULL OR c.visibility = 'public')
        GROUP BY cs.id, u.login_name, c.name, c.slug
        ORDER BY cs.last_activity DESC
        LIMIT $2
        "#,
        viewer_user_id,
        SESSION_LIST_LIMIT,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| SessionWithCounts {
            is_full: row.participant_count >= row.max_participants as i64,
            id: row.id,
            owner_login_name: row.owner_login_name,
            title: row.title,
            width: row.width,
            height: row.height,
            created_at: row.created_at,
            participant_count: row.participant_count,
            max_participants: row.max_participants,
            is_public: row.is_public,
            community_name: row.community_name,
            community_slug: row.community_slug,
            is_owner: row.is_owner,
            viewer_is_participant: row.viewer_is_participant,
            preview_version: None,
        })
        .collect())
}

/// Sessions the viewer owns or is drawing in, public or not.
///
/// Without this a private session is reachable only through a link the owner
/// has to have kept: the lobby lists public sessions only, so closing the tab
/// stranded the canvas.
async fn find_viewer_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    viewer_user_id: Uuid,
) -> Result<Vec<SessionWithCounts>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            cs.id,
            u.login_name AS owner_login_name,
            cs.title,
            cs.width,
            cs.height,
            cs.created_at,
            cs.max_participants,
            cs.is_public,
            c.name AS "community_name?",
            c.slug AS "community_slug?",
            COALESCE(COUNT(DISTINCT csp.user_id) FILTER (WHERE csp.is_active = true), 0) AS "participant_count!",
            cs.owner_id = $1 AS "is_owner!",
            COUNT(*) FILTER (WHERE csp.user_id = $1 AND csp.is_active = true) > 0 AS "viewer_is_participant!"
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        WHERE cs.ended_at IS NULL
        GROUP BY cs.id, u.login_name, c.name, c.slug
        HAVING cs.owner_id = $1
            OR COUNT(*) FILTER (WHERE csp.user_id = $1 AND csp.is_active = true) > 0
        ORDER BY cs.last_activity DESC
        LIMIT $2
        "#,
        viewer_user_id,
        SESSION_LIST_LIMIT,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| SessionWithCounts {
            is_full: row.participant_count >= row.max_participants as i64,
            id: row.id,
            owner_login_name: row.owner_login_name,
            title: row.title,
            width: row.width,
            height: row.height,
            created_at: row.created_at,
            participant_count: row.participant_count,
            max_participants: row.max_participants,
            is_public: row.is_public,
            community_name: row.community_name,
            community_slug: row.community_slug,
            is_owner: row.is_owner,
            viewer_is_participant: row.viewer_is_participant,
            preview_version: None,
        })
        .collect())
}

/// Fills in `preview_version` for a batch of cards, in one round trip.
///
/// A missing preview is not an error: a session nobody has drawn in yet has
/// none, and a Redis that is unwell should cost the lobby its pictures rather
/// than the page.
async fn attach_preview_versions(state: &AppState, sessions: &mut [&mut SessionWithCounts]) {
    let room_uuids: Vec<Uuid> = sessions.iter().map(|session| session.id).collect();
    let versions = preview_versions(state, &room_uuids).await;
    for (session, version) in sessions.iter_mut().zip(versions) {
        session.preview_version = version;
    }
}

/// The two lists the lobby shows: the viewer's own sessions, then the public
/// ones they are not already in. A session in both places is just noise.
async fn lobby_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    viewer_user_id: Option<Uuid>,
) -> Result<(Vec<SessionWithCounts>, Vec<SessionWithCounts>), AppError> {
    let viewer_sessions = match viewer_user_id {
        Some(user_id) => find_viewer_sessions(tx, user_id).await?,
        None => Vec::new(),
    };
    let mut active_sessions = find_public_sessions(tx, viewer_user_id).await?;
    active_sessions.retain(|session| !session.is_owner && !session.viewer_is_participant);
    Ok((viewer_sessions, active_sessions))
}

/// Both lobby lists' previews in one lookup. They are two sections of one
/// page, and a session cannot be in both.
///
/// Called after the transaction is done with, never inside it: this is a round
/// trip to Redis, and holding a database connection across it would be paying
/// for one pool out of another.
async fn attach_lobby_previews(
    state: &AppState,
    viewer_sessions: &mut [SessionWithCounts],
    active_sessions: &mut [SessionWithCounts],
) {
    let mut all: Vec<&mut SessionWithCounts> = viewer_sessions
        .iter_mut()
        .chain(active_sessions.iter_mut())
        .collect();
    attach_preview_versions(state, &mut all).await;
}

/// GET /collaborate/sessions — both session lists on their own, so the lobby
/// can poll them. A session filling up or ending is the one thing on this page
/// that goes stale in seconds.
pub async fn collaborate_sessions_fragment(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let viewer_user_id = auth_session.user.as_ref().map(|u| u.id);

    let mut tx = state.db_pool.begin().await?;
    let (mut viewer_sessions, mut active_sessions) = lobby_sessions(&mut tx, viewer_user_id).await?;
    tx.commit().await?;
    attach_lobby_previews(&state, &mut viewer_sessions, &mut active_sessions).await;

    let template = state
        .env
        .get_template("collaborate_sessions_fragment.jinja")?;
    let rendered = template.render(context! {
        viewer_sessions,
        active_sessions,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

/// GET /api/collaborate/posts — one batch of finished-collaboration cards plus
/// the sentinel that pulls the next. Same contract as the / and /home feeds.
pub async fn load_more_collaborative_posts(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(query): Query<crate::web::handlers::home::LoadMoreQuery>,
) -> Result<impl IntoResponse, AppError> {
    let (viewer_user_id, viewer_show_sensitive) = match auth_session.user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let mut tx = state.db_pool.begin().await?;
    let posts = crate::models::post::find_collaborative_posts(
        &mut tx,
        query.limit,
        query.offset,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    tx.commit().await?;

    let template = state.env.get_template("post_feed_fragment.jinja")?;
    let rendered = template.render(context! {
        feed => crate::web::handlers::home::feed_context(
            posts,
            "/api/collaborate/posts",
            query.offset,
        ),
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn collaborate_lobby(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<LobbyQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Viewable signed out: the lobby doubles as a public gallery. Creating a
    // session still needs an account, which the template gates.
    let user = auth_session.user;
    let (viewer_user_id, viewer_show_sensitive) = match user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, viewer_user_id).await?;

    let (mut viewer_sessions, mut active_sessions) = lobby_sessions(&mut tx, viewer_user_id).await?;

    // Finished collaborative drawings, rendered through the same card template
    // as the home grid and paginated through the same sentinel. Sensitive posts
    // follow the viewer's preference exactly as they do elsewhere.
    let collaborative_posts = crate::models::post::find_collaborative_posts(
        &mut tx,
        crate::web::handlers::home::HOME_POSTS_PER_BATCH,
        0,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;

    // Communities this user may post into. Reuses the rule the "move post"
    // feature already applies — public, unlisted where they have posted,
    // private where they are a member — rather than inventing a second one, so
    // what the picker offers is exactly what create_collaborative_session
    // accepts.
    //
    // Split into three tiers so the ones a user actually draws in are reachable
    // without scrolling past every public community on the site: communities
    // they belong to, then ones they have posted in, then the rest. A member
    // who has also posted appears only in the first tier.
    let mut member_communities: Vec<serde_json::Value> = Vec::new();
    let mut participated_communities: Vec<serde_json::Value> = Vec::new();
    let mut other_communities: Vec<serde_json::Value> = Vec::new();
    if let Some(user_id) = viewer_user_id {
        for community in crate::models::post::get_movable_communities(&mut tx, user_id).await? {
            let entry = serde_json::json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "description": community.description,
                "owner_login_name": community.owner_login_name,
                "visibility": community.visibility,
            });
            if community.is_member {
                member_communities.push(entry);
            } else if community.has_participated {
                participated_communities.push(entry);
            } else {
                other_communities.push(entry);
            }
        }
    }

    tx.commit().await?;
    attach_lobby_previews(&state, &mut viewer_sessions, &mut active_sessions).await;

    let template = state.env.get_template("collaborate_lobby.jinja")?;

    let rendered = template.render(context! {
        current_user => user,
        viewer_sessions => viewer_sessions,
        active_sessions => active_sessions,
        // Shared card fragment contract, sentinel included: the gallery pages
        // through /api/collaborate/posts the way / and /home page through
        // theirs.
        feed => crate::web::handlers::home::feed_context(
            collaborative_posts,
            "/api/collaborate/posts",
            0,
        ),
        // Three tiers rather than one flat list; the template renders them as
        // optgroups in that order.
        has_postable_communities => !member_communities.is_empty()
            || !participated_communities.is_empty()
            || !other_communities.is_empty(),
        member_communities => member_communities,
        participated_communities => participated_communities,
        other_communities => other_communities,
        selected_community_slug => query.community,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        canvas_sizes => canvas_size_options(),
        participant_choices => MAX_PARTICIPANTS_CHOICES,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn create_collaborative_session(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, AppError> {
    let user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("Authentication required"))?;

    // A canvas and a seat count this server did not offer are not a matter of
    // taste: both bound how large a checkpoint of this session can be, and the
    // history ceiling is sized on the assumption that they hold.
    if !CANVAS_SIZES.contains(&(request.width.max(0) as u32, request.height.max(0) as u32)) {
        return Err(AppError::InvalidFormData(format!(
            "unsupported canvas size {}x{}",
            request.width, request.height
        )));
    }
    if !MAX_PARTICIPANTS_CHOICES.contains(&request.max_participants) {
        return Err(AppError::InvalidFormData(format!(
            "unsupported participant limit {}",
            request.max_participants
        )));
    }

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Parse community_id if provided, otherwise None for personal collaborative sessions
    let community_id = request
        .community_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .and_then(|id| id.parse::<Uuid>().ok());

    // A saved session becomes a post in this community, so the same rule that
    // governs posting has to apply here. Without this any id was accepted,
    // which would put a drawing into a private community the caller is not a
    // member of.
    if let Some(community_id) = community_id {
        let allowed = crate::models::post::get_movable_communities(&mut tx, user.id)
            .await?
            .into_iter()
            .any(|community| community.id == community_id);
        if !allowed {
            return Err(AppError::Forbidden);
        }
    }

    let session_id = Uuid::new_v4();
    sqlx::query!(
        r#"
        INSERT INTO collaborative_sessions
        (id, owner_id, title, width, height, is_public, community_id, max_participants)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
        session_id,
        user.id,
        request.title,
        request.width,
        request.height,
        request.is_public,
        community_id,
        request.max_participants
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(CreateSessionResponse {
        session_id: session_id.to_string(),
        url: format!("/collaborate/{}", session_id),
    }))
}

pub async fn save_collaborative_session(
    Path(session_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<SaveSessionResponse>, AppError> {
    let user = auth_session
        .user
        .ok_or_else(|| anyhow::anyhow!("Authentication required"))?;

    let db = &state.db_pool;

    let session = sqlx::query!(
        r#"
        SELECT owner_id, saved_post_id, u.login_name as owner_login_name 
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        WHERE cs.id = $1
        "#,
        session_uuid
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Session not found"))?;

    if session.owner_id != user.id {
        return Err(anyhow::anyhow!("Only session owner can save").into());
    }

    if session.saved_post_id.is_some() {
        return Err(anyhow::anyhow!("Session has already been saved").into());
    }

    let png_data = body.to_vec();

    let (post_id, owner_login_name) =
        db::save_session_to_post(db.clone(), session_uuid, user.id, png_data, state.clone())
            .await
            .map_err(|e| anyhow::anyhow!("Save failed: {}", e))?;

    let post_url = format!("/@{}/{}", owner_login_name, post_id);

    // Note: Session ending and participant notification will be handled by the WebSocket END_SESSION message
    // that the client sends after receiving this HTTP response. This prevents double-broadcasting.

    Ok(Json(SaveSessionResponse {
        post_id: post_id.to_string(),
        owner_login_name,
        post_url,
    }))
}

pub async fn serve_collaborative_app() -> Result<Response, AppError> {
    let html = std::fs::read_to_string("neo-cucumber/dist/index.html")
        .map_err(|_| anyhow::anyhow!("Failed to load collaborative app"))?;
    Ok(Html(html).into_response())
}

pub async fn get_active_sessions_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionWithCounts>>, AppError> {
    let user = match auth_session.user {
        Some(user) => user,
        None => return Err(anyhow::anyhow!("Authentication required").into()),
    };

    let mut tx = state.db_pool.begin().await?;
    // Same list the lobby renders, so the two cannot disagree about which
    // sessions exist. Full ones are included and flagged rather than dropped.
    let mut active_sessions = find_public_sessions(&mut tx, Some(user.id)).await?;
    tx.commit().await?;
    let mut all: Vec<&mut SessionWithCounts> = active_sessions.iter_mut().collect();
    attach_preview_versions(&state, &mut all).await;

    Ok(Json(active_sessions))
}


#[cfg(test)]
mod tests {
    use super::{canvas_size_options, MAX_PARTICIPANTS_CHOICES};
    use crate::web::handlers::test_support;
    use minijinja::context;
    use serde_json::json;

    fn sample_community(id: &str, name: &str, slug: &str, visibility: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": name,
            "slug": slug,
            "description": "",
            "owner_login_name": "owner",
            "visibility": visibility,
        })
    }

    /// A lobby session card's context. Defaults describe an open, public,
    /// half-full session in a community; tests override the field under test.
    fn sample_session(overrides: serde_json::Value) -> serde_json::Value {
        let mut session = json!({
            "id": "00000000-0000-0000-0000-000000000009",
            "owner_login_name": "someone",
            "title": "Doodle",
            "width": 300,
            "height": 300,
            "created_at": "2026-01-02T03:04:05",
            "participant_count": 2,
            "max_participants": 4,
            "is_full": false,
            "is_public": true,
            "community_name": "Open Studio",
            "community_slug": "open",
            "is_owner": false,
            "viewer_is_participant": false,
            "preview_version": null,
        });
        for (key, value) in overrides.as_object().expect("object") {
            session[key] = value.clone();
        }
        session
    }

    fn lobby_context(signed_in: bool, posts: Vec<serde_json::Value>) -> minijinja::Value {
        lobby_context_with_sessions(signed_in, posts, Vec::new(), Vec::new())
    }

    fn lobby_context_with_sessions(
        signed_in: bool,
        posts: Vec<serde_json::Value>,
        active_sessions: Vec<serde_json::Value>,
        viewer_sessions: Vec<serde_json::Value>,
    ) -> minijinja::Value {
        let community = |id, name, slug, visibility| {
            if signed_in {
                vec![sample_community(id, name, slug, visibility)]
            } else {
                vec![]
            }
        };
        let members = community(
            "00000000-0000-0000-0000-000000000003",
            "Private Club",
            "club",
            "private",
        );
        let participated = community(
            "00000000-0000-0000-0000-000000000004",
            "Open Studio",
            "open",
            "public",
        );
        let others = community(
            "00000000-0000-0000-0000-000000000005",
            "Somewhere Else",
            "elsewhere",
            "public",
        );
        context! {
            current_user => if signed_in {
                json!({"login_name": "someone", "email_verified_at": "2026-01-01T00:00:00Z"})
            } else {
                json!(null)
            },
            active_sessions => active_sessions,
            viewer_sessions => viewer_sessions,
            feed => context! {
                posts => posts,
                has_more => false,
                next_url => "",
            },
            canvas_sizes => canvas_size_options(),
            participant_choices => MAX_PARTICIPANTS_CHOICES,
            has_postable_communities => signed_in,
            member_communities => members,
            participated_communities => participated,
            other_communities => others,
            selected_community_slug => "open",
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    fn sample_post() -> serde_json::Value {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "title": "Together",
            "user_login_name": "someone",
            "community_slug": "open",
            "community_name": "Open Studio",
            "image_filename": "abcdef.png",
            "image_width": 300,
            "image_height": 300,
            "is_sensitive": false,
            "published_at": "2026-01-02T03:04:05Z",
        })
    }

    #[test]
    fn lobby_renders_signed_out_without_the_create_form() {
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(false, vec![sample_post()]))
            .expect("renders signed out");
        assert!(!rendered.contains("create-session-form"));
        assert!(rendered.contains("collaborate-sign-in-to-create"));
        // The gallery is the reason signed-out visitors can reach this page.
        assert!(rendered.contains("posts-grid-item"));
    }

    #[test]
    fn create_form_preselects_the_linked_community() {
        // A community page links here with ?community=<slug>; the option for
        // that community must come back selected, or the saved drawing would
        // land nowhere.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("name=\"community_id\""));
        assert!(rendered.contains("collaborate-community-none"));
        assert!(
            rendered.contains("selected"),
            "linked community was not preselected"
        );
    }

    #[test]
    fn lobby_omits_the_live_sessions_section_when_there_are_none() {
        // No empty-state message: with zero live sessions the section is simply
        // absent rather than announcing its own emptiness.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(false, vec![sample_post()]))
            .expect("renders");
        assert!(!rendered.contains("collaborate-active-sessions"));
        assert!(!rendered.contains("no-sessions"));
        // ...while the gallery still renders.
        assert!(rendered.contains("posts-grid-item"));
    }

    #[test]
    fn lobby_shows_the_create_form_when_signed_in() {
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("create-session-form"));
    }

    #[test]
    fn community_picker_ranks_membership_above_participation() {
        // The whole point of the tiers: a user with a hundred public
        // communities in the list still finds their own without scrolling.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        let yours = rendered
            .find("collaborate-community-group-yours")
            .expect("member group rendered");
        let participated = rendered
            .find("collaborate-community-group-participated")
            .expect("participated group rendered");
        let public = rendered
            .find("collaborate-community-group-public")
            .expect("public group rendered");
        assert!(yours < participated && participated < public, "tiers out of order");
        // Non-public tiers say so in the option text, because a saved drawing
        // landing somewhere nobody can see it is a surprise worth preventing.
        assert!(rendered.contains("community-badge-private"));
    }

    #[test]
    fn community_picker_is_one_combobox_over_the_select() {
        // Regression: the picker used to be a search box plus a separate
        // dropdown — two controls for one decision, where typing never got you
        // closer to picking. The select stays as the value holder so there is
        // exactly one community_id and no-JS still works.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("role=\"combobox\""));
        assert!(rendered.contains("id=\"community-listbox\""));
        assert!(!rendered.contains("id=\"community-filter\""));
        assert_eq!(rendered.matches("name=\"community_id\"").count(), 1);
        // Owner handle and slug are matchable, not just the display name.
        assert!(rendered.contains("data-search=\"open studio open @owner"));
    }

    #[test]
    fn create_form_lays_labels_and_controls_out_in_one_grid() {
        // The clutter was four differently-shaped rows and two loose hint
        // paragraphs; every field now sits in the label/control grid and each
        // hint is attached to the field it explains.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("class=\"session-form\""));
        assert_eq!(rendered.matches("class=\"field-help\"").count(), 2);
        // Every control still ships — tidying must not drop a field.
        for id in [
            "canvas-size",
            "session-title",
            "max-participants",
            "is-public",
            "session-community",
        ] {
            assert!(rendered.contains(&format!("id=\"{id}\"")), "{id} missing");
        }
    }

    #[test]
    fn lobby_gallery_carries_the_shared_density_control() {
        // Same control, grid id and head fragment as / and /home, so the
        // stored column count applies here too.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(false, vec![sample_post()]))
            .expect("renders");
        assert!(rendered.contains("id=\"post-cols\""));
        assert!(rendered.contains("id=\"post-feed-grid\""));
        assert!(rendered.contains("class=\"center-wide\""));
        assert!(rendered.contains("class=\"feed-header\""));
    }

    #[test]
    fn session_cards_show_seats_and_destination_community() {
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context_with_sessions(
                false,
                vec![],
                vec![sample_session(json!({}))],
                vec![],
            ))
            .expect("renders");
        assert!(rendered.contains("2 / 4"));
        assert!(rendered.contains("/communities/@open"));
        assert!(rendered.contains("Open Studio"));
    }

    #[test]
    fn session_cards_omit_the_community_line_for_personal_sessions() {
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context_with_sessions(
                false,
                vec![],
                vec![sample_session(json!({
                    "community_name": null,
                    "community_slug": null,
                }))],
                vec![],
            ))
            .expect("renders");
        // The class also appears in the page's <style> block, so match the tag.
        assert!(!rendered.contains("<p class=\"session-community\">"));
    }

    /// A session nobody has drawn in yet has no preview, and a card that
    /// pointed at one anyway would render a broken image on every poll.
    #[test]
    fn session_cards_omit_the_preview_until_one_has_been_uploaded() {
        let env = test_support::env();
        let rendered = env
            .get_template("collaborate_lobby.jinja")
            .expect("template")
            .render(lobby_context_with_sessions(
                true,
                vec![],
                vec![sample_session(json!({}))],
                vec![],
            ))
            .expect("render");
        assert!(!rendered.contains("session-preview"));
        assert!(!rendered.contains("/preview?v="));
    }

    /// The version rides on the URL so a refreshed canvas is a different image
    /// to the browser; without it every card would keep showing whatever it
    /// cached the first time.
    #[test]
    fn session_cards_show_the_preview_at_its_uploaded_version() {
        let env = test_support::env();
        let rendered = env
            .get_template("collaborate_lobby.jinja")
            .expect("template")
            .render(lobby_context_with_sessions(
                true,
                vec![],
                vec![sample_session(json!({"preview_version": 1_700_000_000_123u64}))],
                vec![],
            ))
            .expect("render");
        assert!(rendered.contains(
            "/collaborate/00000000-0000-0000-0000-000000000009/preview?v=1700000000123"
        ));
        // The canvas's own dimensions, so the card reserves the right shape
        // before the image arrives.
        assert!(rendered.contains("width=\"300\""));
        assert!(rendered.contains("height=\"300\""));
    }

    #[test]
    fn full_sessions_stay_listed_without_a_join_link() {
        // Regression: full sessions used to be filtered out of the query, so a
        // busy canvas looked exactly like one that had ended.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context_with_sessions(
                false,
                vec![],
                vec![sample_session(json!({
                    "participant_count": 4,
                    "is_full": true,
                }))],
                vec![],
            ))
            .expect("renders");
        assert!(rendered.contains("4 / 4"));
        assert!(rendered.contains("collaborate-session-full"));
        assert!(!rendered.contains("/collaborate/00000000-0000-0000-0000-000000000009"));
    }

    #[test]
    fn a_full_session_the_viewer_is_already_in_keeps_its_join_link() {
        // The capacity check lets existing participants back in, so dropping
        // their link would lock them out of their own drawing.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context_with_sessions(
                true,
                vec![],
                vec![],
                vec![sample_session(json!({
                    "participant_count": 4,
                    "is_full": true,
                    "viewer_is_participant": true,
                }))],
            ))
            .expect("renders");
        assert!(rendered.contains("/collaborate/00000000-0000-0000-0000-000000000009"));
    }

    #[test]
    fn viewer_sessions_list_reaches_link_only_sessions() {
        // A private session appears nowhere else: the public list filters on
        // is_public, so without this list the owner needs the original URL.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context_with_sessions(
                true,
                vec![],
                vec![],
                vec![sample_session(json!({
                    "is_public": false,
                    "is_owner": true,
                }))],
            ))
            .expect("renders");
        assert!(rendered.contains("collaborate-your-sessions"));
        assert!(rendered.contains("collaborate-session-link-only"));
        assert!(rendered.contains("/collaborate/00000000-0000-0000-0000-000000000009"));
    }

    #[test]
    fn the_sessions_block_polls_itself() {
        // The wrapper carries the poll attributes and swaps outerHTML, so the
        // swapped-in copy keeps polling. Losing them stops the lobby refreshing
        // after the first tick.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_sessions_fragment.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                viewer_sessions => Vec::<serde_json::Value>::new(),
                active_sessions => vec![sample_session(json!({}))],
                ftl_lang => "en",
            })
            .expect("renders");
        assert!(rendered.contains("hx-get=\"/collaborate/sessions\""));
        assert!(rendered.contains("hx-trigger=\"every 15s\""));
        // Morph, not replace: the list repaints every 15s and rebuilding it
        // wholesale dropped focus and hover off cards that had not changed.
        assert!(rendered.contains("hx-swap=\"outerMorph\""));
        assert!(rendered.contains("id=\"collaborate-sessions\""));
    }

    #[test]
    fn lobby_gallery_paginates_through_its_own_endpoint() {
        // Regression: the gallery used to hardcode has_more false, so it capped
        // at one batch. The sentinel must point here, not at the home feed.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(context! {
                current_user => json!(null),
                active_sessions => Vec::<serde_json::Value>::new(),
                viewer_sessions => Vec::<serde_json::Value>::new(),
                feed => crate::web::handlers::home::feed_context(
                    Vec::new(),
                    "/api/collaborate/posts",
                    0,
                ),
                canvas_sizes => canvas_size_options(),
            participant_choices => MAX_PARTICIPANTS_CHOICES,
                has_postable_communities => false,
                selected_community_slug => json!(null),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("renders");
        // An empty batch is short of a full one, so no sentinel — the contract
        // the shared fragment already enforces for / and /home.
        assert!(!rendered.contains("infinite-scroll-sentinel"));

        let fragment = env
            .get_template("post_feed_fragment.jinja")
            .expect("template loads");
        let rendered = fragment
            .render(context! {
                feed => context! {
                    posts => vec![sample_post()],
                    has_more => true,
                    next_url => "/api/collaborate/posts?offset=60&limit=60",
                },
                r2_public_endpoint_url => "https://example.test",
            })
            .expect("renders");
        assert!(rendered.contains("&#x2f;api&#x2f;collaborate&#x2f;posts?offset=60&amp;limit=60"));
    }

    #[test]
    fn lobby_gallery_reuses_the_home_card_and_omits_the_sentinel() {
        // has_more is false, so the shared fragment must not emit a sentinel
        // pointing at /api/home/posts from this page.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(false, vec![sample_post()]))
            .expect("renders");
        assert!(rendered.contains("post-card-byline"));
        assert!(!rendered.contains("infinite-scroll-sentinel"));
    }

    #[test]
    fn create_form_carries_translated_failure_messages() {
        // Regression: the submit handler alerted two hardcoded English strings.
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("data-error=\"collaborate-create-error\""));
        assert!(rendered.contains("data-network-error=\"collaborate-create-network-error\""));
        assert!(!rendered.contains("Failed to create session"));
    }
}
