use crate::app_error::AppError;
use crate::models::community::find_community_by_id;
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::i18n::{ExtractAcceptLanguage, ExtractFtlLang};
use crate::web::presence::{joined_room, Activity, Presence};
use crate::web::rate_limit;
use crate::web::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use minijinja::context;
use tower_sessions::Session;
use uuid::Uuid;

use super::db;
use super::guest::{self, Collaborator};
use super::preview::preview_versions;
use super::types::*;
use super::utils::get_preferred_locale;
use minijinja::value::Serde;

/// The canvases a session can be created with.
///
/// One list, because the lobby form offers these and the create endpoint has to
/// insist on them. It used to only offer them: `width` and `height` went from
/// the request body into the row untouched, so a session could be created at
/// any size at all -- and a session's size is what bounds every checkpoint
/// snapshot of it, which is what `MAX_SNAPSHOT_BYTES` is derived from and what
/// the history ceiling is sized against.
pub const CANVAS_SIZES: [(u32, u32); 3] = [(300, 300), (1024, 768), (1000, 1000)];

/// The one the lobby's form starts on, and what `/oeee-cafe` in Discord
/// opens unless asked otherwise.
pub const DEFAULT_CANVAS_SIZE: (u32, u32) = (1024, 768);

/// The seats a session can offer. A checkpoint holds two snapshots per
/// participant, so this is the other half of what bounds one.
pub const MAX_PARTICIPANTS_CHOICES: [i32; 4] = [2, 4, 6, 8];

/// `CANVAS_SIZES` as the form wants them: the value it posts back, the
/// label a person reads, and whether it is the one chosen to begin with.
fn canvas_size_options() -> Vec<(String, String, bool)> {
    CANVAS_SIZES
        .iter()
        .map(|&(width, height)| {
            (
                format!("{width}x{height}"),
                format!("{width}×{height}"),
                (width, height) == DEFAULT_CANVAS_SIZE,
            )
        })
        .collect()
}

/// Who the collaborative page is: the account, the guest this browser
/// already is, or nobody yet -- which the page answers by asking for a name.
pub async fn get_auth_info(
    auth_session: AuthSession,
    session: Session,
    State(state): State<AppState>,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
) -> Result<Json<AuthInfo>, AppError> {
    let preferred_language = auth_session
        .user
        .as_ref()
        .and_then(|user| user.preferred_language.clone());
    let preferred_locale = get_preferred_locale(preferred_language, &accept_language);
    let who = guest::current(auth_session.user.as_ref(), &session, &state.db_pool).await?;
    Ok(Json(AuthInfo {
        user_id: who.as_ref().map(|who| who.id().to_string()),
        login_name: who.as_ref().map(|who| who.name().to_string()),
        is_guest: who.as_ref().is_none_or(Collaborator::is_guest),
        preferred_locale,
    }))
}

/// How many guests one address may make, or rename, in [`GUEST_WINDOW`].
/// A person makes one and renames it now and then; a classroom behind one
/// address makes thirty.
const GUESTS_PER_WINDOW: i64 = 40;
const GUEST_WINDOW: u64 = 60 * 60;

#[derive(serde::Deserialize)]
pub struct GuestForm {
    nickname: String,
}

/// POST /collaborate/guest: makes this browser a guest called `nickname`, or
/// renames the guest it is. Somebody signed in is already somebody.
pub async fn become_collaboration_guest(
    auth_session: AuthSession,
    session: Session,
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(form): Json<GuestForm>,
) -> Result<Response, AppError> {
    if auth_session.user.is_some() {
        return Err(AppError::BadRequest(
            "signed in already; a guest is for somebody without an account".to_string(),
        ));
    }
    if !guest_admitted(&state, &headers).await {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let who = guest::become_guest(&session, &state.db_pool, &form.nickname).await?;
    Ok(Json(AuthInfo {
        user_id: Some(who.id().to_string()),
        login_name: Some(who.name().to_string()),
        is_guest: true,
        preferred_locale: String::new(),
    })
    .into_response())
}

/// Whether this address may make another guest or another guest's session.
async fn guest_admitted(state: &AppState, headers: &HeaderMap) -> bool {
    let ip = rate_limit::client_ip(headers).unwrap_or_else(|| "unknown".to_string());
    rate_limit::admitted(
        &state.redis_pool,
        &format!("oeee:collaborate-guest-rate:{ip}"),
        GUESTS_PER_WINDOW,
        GUEST_WINDOW,
    )
    .await
}

pub async fn get_collaboration_meta(
    Path(session_uuid): Path<Uuid>,
    auth_session: AuthSession,
    session: Session,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let db = &state.db_pool;
    let who = guest::current(auth_session.user.as_ref(), &session, db).await?;
    let ip = rate_limit::client_ip(&headers);

    // Sent out by the owner: said as such, so the page tells them they were
    // removed rather than that something failed.
    if db::viewer_is_banned(db, session_uuid, who.as_ref(), ip.as_deref()).await? {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "removed" })),
        )
            .into_response());
    }

    // Somebody with no name yet is asked whether they could come in as a
    // guest, which is what they would be.
    if !db::viewer_may_enter(db, session_uuid, who.as_ref(), ip.as_deref()).await? {
        return Err(match who {
            Some(_) => AppError::Forbidden,
            None => AppError::Unauthorized,
        });
    }

    // Ended sessions answer too. The page opens on this, and for a session
    // that is over it has to learn where the post is, or that there is none,
    // rather than fail to load: a link shared earlier, a lobby card clicked
    // in the moment before it went, the owner reloading after a save.
    let session = sqlx::query!(
        r#"
        SELECT cs.title, cs.width, cs.height, cs.owner_participant_id, cs.saved_post_id,
               cs.max_participants, cs.ended_at,
               COALESCE(u.login_name, g.nickname) AS "owner_login_name!",
               cs.owner_guest_id IS NOT NULL AS "owner_is_guest!",
               cs.allow_guests AND (c.id IS NULL OR c.visibility = 'public') AS "guests_welcome!"
        FROM collaborative_sessions cs
        LEFT JOIN users u ON cs.owner_id = u.id
        LEFT JOIN collaborative_guests g ON cs.owner_guest_id = g.id
        LEFT JOIN communities c ON cs.community_id = c.id
        WHERE cs.id = $1
        "#,
        session_uuid
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Session".to_string()))?;

    let user_count = db::get_active_user_count(db, session_uuid).await?;

    let saved_post_url = db::saved_post_path(db, session_uuid).await?;

    Ok(Json(CollaborationMeta {
        title: session
            .title
            .unwrap_or_else(|| "Untitled Collaboration".to_string()),
        width: session.width,
        height: session.height,
        owner_id: session.owner_participant_id.to_string(),
        saved_post_id: session.saved_post_id.map(|id| id.to_string()),
        saved_post_url,
        owner_login_name: session.owner_login_name,
        owner_is_guest: session.owner_is_guest,
        guests_welcome: session.guests_welcome,
        max_users: session.max_participants,
        current_user_count: user_count,
        ended: session.ended_at.is_some(),
    })
    .into_response())
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
    viewer_id: Option<Uuid>,
) -> Result<Vec<SessionWithCounts>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            cs.id,
            COALESCE(u.login_name, g.nickname) AS "owner_login_name!",
            cs.owner_guest_id IS NOT NULL AS "owner_is_guest!",
            cs.title,
            cs.width,
            cs.height,
            cs.created_at,
            cs.max_participants,
            cs.is_public,
            c.name AS "community_name?",
            c.slug AS "community_slug?",
            COALESCE(COUNT(DISTINCT csp.participant_id) FILTER (WHERE csp.is_active = true), 0) AS "participant_count!",
            -- COALESCE, not a bare comparison: $1 is NULL for a visitor who
            -- is not even a guest, and NULL decoded into a non-null bool
            -- fails the query.
            COALESCE(cs.owner_participant_id = $1, false) AS "is_owner!",
            COUNT(*) FILTER (WHERE csp.participant_id = $1 AND csp.is_active = true) > 0 AS "viewer_is_participant!"
        FROM collaborative_sessions cs
        LEFT JOIN users u ON cs.owner_id = u.id
        LEFT JOIN collaborative_guests g ON cs.owner_guest_id = g.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        WHERE cs.is_public = true
          AND cs.ended_at IS NULL
          AND (cs.community_id IS NULL OR c.visibility = 'public')
        GROUP BY cs.id, u.login_name, g.nickname, c.name, c.slug
        ORDER BY cs.last_activity DESC
        LIMIT $2
        "#,
        viewer_id,
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
            owner_is_guest: row.owner_is_guest,
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
    viewer_id: Uuid,
) -> Result<Vec<SessionWithCounts>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            cs.id,
            COALESCE(u.login_name, g.nickname) AS "owner_login_name!",
            cs.owner_guest_id IS NOT NULL AS "owner_is_guest!",
            cs.title,
            cs.width,
            cs.height,
            cs.created_at,
            cs.max_participants,
            cs.is_public,
            c.name AS "community_name?",
            c.slug AS "community_slug?",
            COALESCE(COUNT(DISTINCT csp.participant_id) FILTER (WHERE csp.is_active = true), 0) AS "participant_count!",
            cs.owner_participant_id = $1 AS "is_owner!",
            COUNT(*) FILTER (WHERE csp.participant_id = $1 AND csp.is_active = true) > 0 AS "viewer_is_participant!"
        FROM collaborative_sessions cs
        LEFT JOIN users u ON cs.owner_id = u.id
        LEFT JOIN collaborative_guests g ON cs.owner_guest_id = g.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        WHERE cs.ended_at IS NULL
        GROUP BY cs.id, u.login_name, g.nickname, c.name, c.slug
        HAVING cs.owner_participant_id = $1
            OR COUNT(*) FILTER (WHERE csp.participant_id = $1 AND csp.is_active = true) > 0
        ORDER BY cs.last_activity DESC
        LIMIT $2
        "#,
        viewer_id,
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
            owner_is_guest: row.owner_is_guest,
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
/// `viewer_id` is a participant id, so a guest sees their own sessions too.
async fn lobby_sessions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    viewer_id: Option<Uuid>,
) -> Result<(Vec<SessionWithCounts>, Vec<SessionWithCounts>), AppError> {
    let viewer_sessions = match viewer_id {
        Some(viewer_id) => find_viewer_sessions(tx, viewer_id).await?,
        None => Vec::new(),
    };
    let mut active_sessions = find_public_sessions(tx, viewer_id).await?;
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
    session: Session,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let viewer_id = guest::current(auth_session.user.as_ref(), &session, &state.db_pool)
        .await?
        .map(|who| who.id());

    let mut tx = state.db_pool.begin().await?;
    let (mut viewer_sessions, mut active_sessions) = lobby_sessions(&mut tx, viewer_id).await?;
    tx.commit().await?;
    attach_lobby_previews(&state, &mut viewer_sessions, &mut active_sessions).await;

    let rendered = state
        .render(
            "collaborate_sessions_fragment.jinja",
            context! {
                viewer_sessions => Serde(viewer_sessions),
                active_sessions => Serde(active_sessions),
                ftl_lang,
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

/// GET /api/collaborate/posts — one batch of finished-collaboration cards plus
/// the sentinel that pulls the next. Same contract as Home's feeds.
pub async fn load_more_collaborative_posts(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
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
        query.batch_limit(),
        query.offset,
        viewer_user_id,
        viewer_show_sensitive,
    )
    .await?;
    tx.commit().await?;

    let rendered = state
        .render(
            "post_feed_fragment.jinja",
            context! {
                feed => crate::web::handlers::home::feed_context(
                    posts,
                    "/api/collaborate/posts",
                    query.offset,
                    query.period.as_deref(),
                ),
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
                ftl_lang,
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

pub async fn collaborate_lobby(
    auth_session: AuthSession,
    session: Session,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<LobbyQuery>,
) -> Result<impl IntoResponse, AppError> {
    // Viewable signed out: the lobby doubles as a public gallery, and anybody
    // may open a session from it -- somebody without an account as a guest,
    // naming themselves in the form.
    let user = auth_session.user;
    let (viewer_user_id, viewer_show_sensitive) = match user.as_ref() {
        Some(user) => (Some(user.id), user.show_sensitive_content),
        None => (None, false),
    };
    let db = &state.db_pool;
    let viewer = guest::current(user.as_ref(), &session, db).await?;
    let guest_nickname = viewer
        .as_ref()
        .filter(|viewer| viewer.is_guest())
        .map(|viewer| viewer.name().to_string());

    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, user.as_ref(), &ftl_lang).await?;

    let (mut viewer_sessions, mut active_sessions) =
        lobby_sessions(&mut tx, viewer.as_ref().map(Collaborator::id)).await?;

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
    //
    // Without an account, the public ones: what a guest may open a session in
    // (`postable_communities`).
    let mut member_communities: Vec<serde_json::Value> = Vec::new();
    let mut participated_communities: Vec<serde_json::Value> = Vec::new();
    let mut other_communities: Vec<serde_json::Value> = Vec::new();
    {
        for community in postable_communities(&mut tx, viewer_user_id).await? {
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

    let template = "collaborate_lobby.jinja";

    let rendered = state
        .render_page(
            template,
            common_ctx,
            context! {
                viewer_sessions => Serde(viewer_sessions),
                active_sessions => Serde(active_sessions),
                // Shared card fragment contract, sentinel included: the gallery pages
                // through /api/collaborate/posts the way Home's feeds page through
                // theirs.
                feed => crate::web::handlers::home::feed_context(
                    collaborative_posts,
                    "/api/collaborate/posts",
                    0,
                    None,
                ),
                // Three tiers rather than one flat list; the template renders them as
                // optgroups in that order.
                has_postable_communities => !member_communities.is_empty()
                    || !participated_communities.is_empty()
                    || !other_communities.is_empty(),
                member_communities => Serde(member_communities),
                participated_communities => Serde(participated_communities),
                other_communities => Serde(other_communities),
                selected_community_slug => query.community,
                r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
                canvas_sizes => canvas_size_options(),
                participant_choices => MAX_PARTICIPANTS_CHOICES,
                // The name a guest already goes by, to start the form's name
                // field on. None for an account, and for somebody new.
                guest_nickname,
                nickname_max_chars => guest::NICKNAME_MAX_CHARS,
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

/// The lobby's new-session form, as the browser sends it: the canvas as one
/// "WxH" choice, the checkbox present or absent.
#[derive(serde::Deserialize)]
pub struct CreateSessionForm {
    title: Option<String>,
    #[serde(rename = "canvas-size")]
    canvas_size: String,
    is_public: Option<String>,
    max_participants: i32,
    community_id: Option<String>,
    /// Present when the box is ticked. The form starts it ticked.
    allow_guests: Option<String>,
    /// What somebody without an account will be called. Ignored for an
    /// account.
    nickname: Option<String>,
}

/// The communities somebody may open a session in: where the account may
/// post (`get_movable_communities`), and for anybody else the public ones --
/// asked as a nil user, who has posted nowhere and is a member of nothing.
async fn postable_communities(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Option<Uuid>,
) -> Result<Vec<crate::models::post::PostableCommunity>, AppError> {
    let communities =
        crate::models::post::get_movable_communities(tx, user_id.unwrap_or(Uuid::nil())).await?;
    Ok(match user_id {
        Some(_) => communities,
        None => communities
            .into_iter()
            .filter(|community| {
                community.visibility == crate::models::community::CommunityVisibility::Public
            })
            .collect(),
    })
}

/// POST /collaborate: a session, from the lobby's form. Sent by htmx
/// (collaborate_lobby.jinja), which is told where the new session is with
/// HX-Redirect; sent without script, an ordinary redirect there. A refusal
/// is htmx's error banner's to show (web::htmx).
pub async fn create_collaborative_session_form(
    auth_session: AuthSession,
    session: Session,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::Form(form): axum::Form<CreateSessionForm>,
) -> Result<Response, AppError> {
    // An account opens it as itself. Anybody else opens it as a guest, made
    // or renamed from the form's name field, and only so often.
    let who = match auth_session.user.as_ref() {
        Some(user) => Collaborator::from_user(user),
        None => {
            if !guest_admitted(&state, &headers).await {
                return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
            }
            let nickname = form.nickname.as_deref().unwrap_or_default();
            match guest::current(None, &session, &state.db_pool).await? {
                Some(guest) if nickname.trim().is_empty() || nickname.trim() == guest.name() => {
                    guest
                }
                _ => guest::become_guest(&session, &state.db_pool, nickname).await?,
            }
        }
    };
    let (width, height) = form
        .canvas_size
        .split_once('x')
        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
        .ok_or_else(|| {
            AppError::BadRequest(format!("unsupported canvas size {}", form.canvas_size))
        })?;
    let request = CreateSessionRequest {
        title: form.title.filter(|title| !title.trim().is_empty()),
        width,
        height,
        is_public: form.is_public.is_some(),
        max_participants: form.max_participants,
        community_id: form.community_id,
        allow_guests: form.allow_guests.is_some(),
    };
    let session_id = insert_session(&state, &who, request).await?;
    let url = format!("/collaborate/{}", session_id);
    if headers.get("HX-Request").is_some() {
        Ok(([("HX-Redirect", url)],).into_response())
    } else {
        Ok(axum::response::Redirect::to(&url).into_response())
    }
}

/// Checks a session's settings against what this server offers and who may
/// post where, and records it. The lobby's form opens one here, and so does
/// `/draw` in Discord (discord_interactions.rs).
pub(crate) async fn insert_session(
    state: &AppState,
    owner: &Collaborator,
    request: CreateSessionRequest,
) -> Result<Uuid, AppError> {
    // A canvas and a seat count this server did not offer are not a matter of
    // taste: both bound how large a checkpoint of this session can be, and the
    // history ceiling is sized on the assumption that they hold.
    if !CANVAS_SIZES.contains(&(request.width.max(0) as u32, request.height.max(0) as u32)) {
        return Err(AppError::BadRequest(format!(
            "unsupported canvas size {}x{}",
            request.width, request.height
        )));
    }
    if !MAX_PARTICIPANTS_CHOICES.contains(&request.max_participants) {
        return Err(AppError::BadRequest(format!(
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
    // member of. A guest may open one only in a public community.
    if let Some(community_id) = community_id {
        let allowed = postable_communities(&mut tx, owner.user_id())
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
        (id, owner_id, owner_guest_id, title, width, height, is_public, community_id,
         max_participants, allow_guests)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#,
        session_id,
        owner.user_id(),
        owner.guest_id(),
        request.title,
        request.width,
        request.height,
        request.is_public,
        community_id,
        request.max_participants,
        request.allow_guests,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(session_id)
}

/// The largest PNG a save accepts. Sized like `/draw/finish`: the largest
/// canvas is a few megabytes raw, and a busy drawing does not always compress
/// below the framework's two-megabyte default, which answered such a save
/// with a 413 the owner could do nothing about.
pub const MAX_SAVE_BYTES: usize = 10 * 1024 * 1024;

pub async fn save_collaborative_session(
    Path(session_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;

    // A post goes out under an account. A guest owner keeps the drawing on
    // their device instead (`end_collaborative_session`), and saves it from
    // there once they have signed in -- which makes the session theirs.
    let session = sqlx::query!(
        r#"
        SELECT owner_id, saved_post_id, width, height, ended_at IS NULL AS "open!"
        FROM collaborative_sessions cs
        LEFT JOIN users u ON cs.owner_id = u.id
        WHERE cs.id = $1
        "#,
        session_uuid
    )
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Session".to_string()))?;

    if session.owner_id != Some(user.id) {
        return Err(AppError::Forbidden);
    }
    let owner_login_name = user.login_name.to_string();

    // Already a post: not an error, and the answer is where it is. A save
    // retried after a lost response used to come back as a failure, with the
    // owner left to reload.
    if let Some(post_id) = session.saved_post_id {
        let post_url = db::saved_post_path(db, session_uuid)
            .await?
            .unwrap_or_else(|| format!("/@{}/{}", owner_login_name, post_id));
        return Ok((
            StatusCode::CONFLICT,
            Json(SaveSessionResponse {
                post_id: post_id.to_string(),
                post_url,
                owner_login_name,
            }),
        )
            .into_response());
    }

    // The bytes become a public image and an `images` row of the session's
    // size, so they have to be a PNG of that size before anything is written.
    match super::preview::inspect_image(&body) {
        Some((super::preview::ImageKind::Png, width, height))
            if (width, height) == (session.width as u32, session.height as u32) => {}
        Some((_, width, height)) => {
            return Err(AppError::BadRequest(format!(
                "expected a {}x{} PNG, got {}x{}",
                session.width, session.height, width, height
            )));
        }
        None => {
            return Err(AppError::BadRequest("the body is not a PNG".to_string()));
        }
    }

    let (post_id, owner_login_name) = db::save_session_to_post(
        db.clone(),
        session_uuid,
        user.id,
        body.to_vec(),
        state.clone(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Save failed: {}", e))?;

    // Where the post lives now that it exists, community and all.
    let post_url = db::saved_post_path(db, session_uuid)
        .await?
        .unwrap_or_else(|| format!("/@{}/{}", owner_login_name, post_id));

    // The session is over the moment the post exists, whether or not the
    // owner's socket is still there to say so. One that was over already --
    // timed out, or kept on a guest owner's device and saved from their
    // drafts since -- was sealed and cleaned up when it ended, and doing that
    // again would seal an empty room over its recording.
    if session.open {
        super::messages::finish_session(&state, session_uuid, user.id, &post_url, "system").await;
    }

    Ok(Json(SaveSessionResponse {
        post_id: post_id.to_string(),
        owner_login_name,
        post_url,
    })
    .into_response())
}

/// Who is asking about a room they are in, and whether it is theirs.
async fn room_owner(
    state: &AppState,
    auth_session: &AuthSession,
    session: &Session,
    session_uuid: Uuid,
) -> Result<Collaborator, AppError> {
    let who = guest::current(auth_session.user.as_ref(), session, &state.db_pool)
        .await?
        .ok_or(AppError::Unauthorized)?;
    let owner = sqlx::query_scalar!(
        "SELECT owner_participant_id FROM collaborative_sessions WHERE id = $1",
        session_uuid
    )
    .fetch_optional(&state.db_pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Session".to_string()))?;
    if owner != who.id() {
        return Err(AppError::Forbidden);
    }
    Ok(who)
}

/// POST /collaborate/{uuid}/end: the owner closes the session without making
/// a post of it, and everybody in it is told so.
///
/// How a guest owner finishes: the drawing goes into the drafts on their
/// device, the way a guest's `/draw` does, and from the drafts page it is
/// saved once they have an account (the sign-in makes the session theirs, and
/// `POST /collaborate/{uuid}` takes a session that has ended). An account may
/// end one this way too, rather than leave it to time out.
pub async fn end_collaborative_session(
    Path(session_uuid): Path<Uuid>,
    auth_session: AuthSession,
    session: Session,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let owner = room_owner(&state, &auth_session, &session, session_uuid).await?;
    let open = sqlx::query_scalar!(
        "SELECT ended_at IS NULL AS \"open!\" FROM collaborative_sessions WHERE id = $1",
        session_uuid
    )
    .fetch_one(&state.db_pool)
    .await?;
    if open {
        super::messages::finish_session(&state, session_uuid, owner.id(), "", "system").await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// POST /collaborate/{uuid}/participants/{participant_id}/ban: the owner
/// sends somebody out of the session and keeps them out of it. Their sockets
/// are closed wherever they are held; a guest's address is kept with the ban
/// while they are still connected to give one up by.
pub async fn ban_collaborative_participant(
    Path((session_uuid, participant_id)): Path<(Uuid, Uuid)>,
    auth_session: AuthSession,
    session: Session,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let owner = room_owner(&state, &auth_session, &session, session_uuid).await?;
    if participant_id == owner.id() {
        return Err(AppError::BadRequest(
            "the owner cannot ban themselves".to_string(),
        ));
    }
    let is_guest = sqlx::query_scalar!(
        r#"
        SELECT guest_id IS NOT NULL AS "is_guest!"
        FROM collaborative_sessions_participants
        WHERE session_id = $1 AND participant_id = $2
        "#,
        session_uuid,
        participant_id,
    )
    .fetch_optional(&state.db_pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Participant".to_string()))?;
    let ip = if is_guest {
        super::websocket::participant_ip(&state, session_uuid, participant_id).await
    } else {
        None
    };
    db::ban_participant(&state.db_pool, session_uuid, participant_id, ip.as_deref()).await?;
    let closed = super::websocket::remove_from_room(&state, session_uuid, participant_id).await;
    tracing::info!(
        "Owner {} banned {} from session {} ({} sockets closed)",
        owner.name(),
        participant_id,
        session_uuid,
        closed
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Deserialize)]
pub struct DiscordJoin {
    #[serde(default)]
    secret: String,
}

/// Where Discord's phone app sends a friend who accepted an invitation into
/// a room: the Developer Portal's Deep Link URL is the site, to which Discord
/// adds `/_discord/join?secret=`, the secret being the room's `join`
/// (presence.rs). So on to the room, where everything else is decided as for
/// anyone holding its link; the iOS app opens this itself, as every page.
/// Anything that is not a room's path goes nowhere: Discord hands over
/// whatever the inviter's app said.
pub async fn discord_join(Query(query): Query<DiscordJoin>) -> Result<Response, AppError> {
    let room = joined_room(&query.secret).ok_or_else(|| AppError::NotFound("Room".to_string()))?;
    Ok(axum::response::Redirect::to(&format!("/collaborate/{room}")).into_response())
}

/// The collaborative drawing app: a page built by Vite and served as a file,
/// with the site's toolbar put above it on the way out, so a drawing session
/// has the same title bar as every other page.
pub async fn serve_collaborative_app(
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    auth_session: AuthSession,
    session_id: Option<Path<Uuid>>,
) -> Result<Response, AppError> {
    let html = std::fs::read_to_string("neo-cucumber/dist/index.html")
        .map_err(|_| anyhow::anyhow!("Failed to load collaborative app"))?;

    let mut tx = state.db_pool.begin().await?;
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;
    // In a room: drawing together, grouped with friends in the same room,
    // and the community named if it is public, and while the room is open,
    // how a friend joins it. The lobby is browsing.
    let presence = match session_id {
        Some(Path(session_id)) => {
            let community_id = sqlx::query_scalar!(
                "SELECT community_id FROM collaborative_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
            let community = match community_id {
                Some(id) => find_community_by_id(&mut tx, id).await?,
                None => None,
            };
            let presence = Presence::new(Activity::Collaborating)
                .in_community(community.as_ref())
                .in_room(session_id);
            // None once the room has ended, which nobody can join.
            Some(
                match db::get_session_info(&state.db_pool, session_id).await? {
                    Some(open) => presence.opened(open.opened_at).joinable(
                        session_id,
                        open.max_participants,
                        community.as_ref(),
                    ),
                    None => presence,
                },
            )
        }
        None => None,
    };
    tx.commit().await?;
    let chrome = common_ctx.merge(context! { presence => Serde(presence) });
    let head = state
        .render("collaborate_chrome_head.jinja", chrome.clone())
        .await?;
    // The site's alert and confirmation, which the room says its failures
    // through (frontend/shared/siteDialog.ts), with the toolbar.
    let toolbar = state.render("toolbar.jinja", chrome.clone()).await?
        + &state.render("confirm_dialog.jinja", chrome).await?;

    Ok(Html(with_site_chrome(&html, &head, &toolbar)).into_response())
}

/// `html` with `head` added at the end of its <head> and `toolbar` at the
/// start of its <body>. A page missing either tag comes back without that
/// part rather than with it somewhere else.
fn with_site_chrome(html: &str, head: &str, toolbar: &str) -> String {
    let mut out = html.to_string();
    if let Some(at) = out.find("</head>") {
        out.insert_str(at, head);
    }
    if let Some(body) = out.find("<body")
        && let Some(close) = out[body..].find('>')
    {
        out.insert_str(body + close + 1, toolbar);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{canvas_size_options, MAX_PARTICIPANTS_CHOICES};
    use crate::web::handlers::test_support;
    use minijinja::context;
    use minijinja::value::Serde;
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
            current_user => Serde(if signed_in {
                json!({"login_name": "someone", "email_verified_at": "2026-01-01T00:00:00Z"})
            } else {
                json!(null)
            }),
            active_sessions => Serde(active_sessions),
            viewer_sessions => Serde(viewer_sessions),
            feed => context! {
                posts => Serde(posts),
                has_more => false,
                next_url => "",
            },
            canvas_sizes => canvas_size_options(),
            participant_choices => MAX_PARTICIPANTS_CHOICES,
            has_postable_communities => signed_in,
            member_communities => Serde(members),
            participated_communities => Serde(participated),
            other_communities => Serde(others),
            selected_community_slug => "open",
            guest_nickname => if signed_in { None } else { Some("miro") },
            nickname_max_chars => super::guest::NICKNAME_MAX_CHARS,
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

    /// Somebody without an account may open a session as a guest, so the form
    /// is there for them too -- with a name field an account does not get.
    #[test]
    fn lobby_offers_a_guest_the_create_form_with_a_name() {
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(false, vec![sample_post()]))
            .expect("renders signed out");
        assert!(rendered.contains("create-session-form"));
        assert!(rendered.contains(r#"name="nickname""#));
        assert!(
            rendered.contains(r#"value="miro""#),
            "the guest's name is kept"
        );
        assert!(rendered.contains(r#"name="allow_guests" checked"#));
        assert!(rendered.contains("posts-grid-item"));

        let signed_in = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(!signed_in.contains(r#"name="nickname""#));
        assert!(signed_in.contains(r#"name="allow_guests" checked"#));
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
    fn the_new_session_form_starts_on_1024_by_768_and_offers_1000_by_1000() {
        let env = test_support::env();
        let rendered = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads")
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains(r#"<option value="1024x768" selected>1024×768</option>"#));
        assert!(rendered.contains(r#"<option value="1000x1000">1000×1000</option>"#));
        assert!(rendered.contains(r#"<option value="300x300">300×300</option>"#));
        let sizes = &rendered[rendered.find(r#"id="canvas-size""#).unwrap()..];
        let sizes = &sizes[..sizes.find("</select>").unwrap()];
        assert_eq!(sizes.matches(" selected>").count(), 1);
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
        assert!(
            yours < participated && participated < public,
            "tiers out of order"
        );
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
        assert_eq!(rendered.matches("class=\"field-help\"").count(), 3);
        // Every control still ships — tidying must not drop a field.
        for id in [
            "canvas-size",
            "session-title",
            "max-participants",
            "is-public",
            "allow-guests",
            "session-community",
        ] {
            assert!(rendered.contains(&format!("id=\"{id}\"")), "{id} missing");
        }
    }

    /// The toolbar goes first in the body and the chrome's head at the end of
    /// the head, whatever the built page's tags carry.
    #[test]
    fn the_drawing_app_is_sent_with_the_site_chrome() {
        let page = "<!DOCTYPE html><html><head><title>x</title></head><body class=\"a\"><div id=\"root\"></div></body></html>";
        let out = super::with_site_chrome(
            page,
            "<link rel=\"stylesheet\">",
            "<nav class=\"nav-bar\"></nav>",
        );
        assert!(out.contains("<link rel=\"stylesheet\"></head>"), "{out}");
        assert!(
            out.contains("<body class=\"a\"><nav class=\"nav-bar\"></nav><div id=\"root\">"),
            "{out}"
        );
        // A page without the tags is left alone rather than mangled.
        assert_eq!(
            super::with_site_chrome("<div></div>", "h", "t"),
            "<div></div>"
        );
    }

    #[test]
    fn lobby_gallery_is_the_shared_drawing_grid() {
        // The same grid and per-row control as Home's feeds, so the number a
        // reader chose applies here too.
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
        assert!(rendered.contains(r#"href="/@open""#));
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

    /// A guest who opened a session has no profile, and the name on the card
    /// is one they typed: shown as theirs, marked as a guest's, linked nowhere.
    #[test]
    fn a_guests_session_card_names_them_without_a_profile_link() {
        let env = test_support::env();
        let rendered = env
            .get_template("collaborate_lobby.jinja")
            .expect("template")
            .render(lobby_context_with_sessions(
                false,
                vec![],
                vec![sample_session(json!({
                    "owner_login_name": "a_guest_named_this",
                    "owner_is_guest": true,
                }))],
                vec![],
            ))
            .expect("renders");
        assert!(rendered.contains("a_guest_named_this <span class=\"ds-guest-badge\">"));
        assert!(!rendered.contains("@a_guest_named_this"));
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
                vec![sample_session(
                    json!({"preview_version": 1_700_000_000_123u64}),
                )],
                vec![],
            ))
            .expect("render");
        assert!(rendered
            .contains("/collaborate/00000000-0000-0000-0000-000000000009/preview?v=1700000000123"));
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
                viewer_sessions => Serde(Vec::<serde_json::Value>::new()),
                active_sessions => Serde(vec![sample_session(json!({}))]),
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
                current_user => Serde(json!(null)),
                active_sessions => Serde(Vec::<serde_json::Value>::new()),
                viewer_sessions => Serde(Vec::<serde_json::Value>::new()),
                feed => crate::web::handlers::home::feed_context(
                    Vec::new(),
                    "/api/collaborate/posts",
                    0,
                    None,
                ),
                canvas_sizes => canvas_size_options(),
            participant_choices => MAX_PARTICIPANTS_CHOICES,
                has_postable_communities => false,
                selected_community_slug => Serde(json!(null)),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("renders");
        // An empty batch is short of a full one, so no sentinel — the contract
        // the shared fragment already enforces for Home's feeds.
        assert!(!rendered.contains("infinite-scroll-sentinel"));

        let fragment = env
            .get_template("post_feed_fragment.jinja")
            .expect("template loads");
        let rendered = fragment
            .render(context! {
                feed => context! {
                    posts => Serde(vec![sample_post()]),
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
    fn create_form_is_sent_by_htmx_with_a_translated_network_failure() {
        // The form is htmx's to send, and a refusal is the error banner's to
        // show; a request that got no answer is the one it says itself, in
        // the reader's language (it once alerted hardcoded English).
        let env = test_support::env();
        let template = env
            .get_template("collaborate_lobby.jinja")
            .expect("template loads");
        let rendered = template
            .render(lobby_context(true, vec![]))
            .expect("renders signed in");
        assert!(rendered.contains("hx-post=\"/collaborate\""));
        assert!(rendered.contains("data-network-error=\"collaborate-create-network-error\""));
        assert!(!rendered.contains("Failed to create session"));
    }
}
