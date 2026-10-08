//! Collaborative sessions: the list, their recordings and replaying them.

use crate::app_error::AppError;
use crate::models::admin::{find_all_collaborative_sessions, AdminSessionStatus, AdminSort};
use crate::web::context::CommonContext;
use crate::web::handlers::collaborate::preview::preview_versions;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use minijinja::value::Serde;
use serde::Deserialize;
use uuid::Uuid;

const SESSIONS_PER_PAGE: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct AdminSessionsQuery {
    pub page: Option<i64>,
    #[serde(default)]
    pub sort: AdminSort,
    /// Omit to show everything. A value that is not one of the three is a 400
    /// rather than a fallback -- `serde(default)` covers a missing key, not an
    /// unknown one -- which is the same deal `sort` above has offered since it
    /// was written.
    #[serde(default)]
    pub status: AdminSessionStatus,
}

/// GET /admin/collaborative-sessions — every session, link-only and
/// private-community ones included, live or ended.
///
/// The lobby can only ever show a person their own sessions and the public
/// ones, which leaves no way at all to see a room that is filling up out of
/// sight. Each live one carries the same participant-rendered preview the
/// lobby cards use, so what is being drawn is visible without joining and
/// taking a seat.
pub async fn admin_collaborative_sessions(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<AdminSessionsQuery>,
) -> Result<Html<String>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * SESSIONS_PER_PAGE;

    let mut tx = state.db_pool.begin().await?;
    let mut sessions = find_all_collaborative_sessions(
        &mut tx,
        query.sort,
        query.status,
        SESSIONS_PER_PAGE,
        offset,
    )
    .await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    // After the transaction, never inside it: this is a round trip to Redis,
    // and holding a database connection across it would be paying for one pool
    // out of another.
    let room_uuids: Vec<Uuid> = sessions.iter().map(|session| session.id).collect();
    for (session, version) in sessions
        .iter_mut()
        .zip(preview_versions(&state, &room_uuids).await)
    {
        session.preview_version = version;
    }

    let has_next = sessions.len() as i64 == SESSIONS_PER_PAGE;
    let rendered = state
        .render_page(
            "admin/collaborative_sessions.jinja",
            common_ctx,
            context! {
                sessions => Serde(sessions),
                page => page,
                sort => Serde(query.sort),
                status => Serde(query.status),
                has_next => has_next,
            },
        )
        .await?;

    Ok(Html(rendered))
}

/// GET /admin/collaborative-sessions/:uuid/archive — the whole recording of
/// one session, as one file.
///
/// The point of holding it is to run it back through the client and watch
/// where the canvas goes wrong, which is what a post-mortem of a desync
/// actually needs -- reconstructing one out of whatever survived in Redis an
/// hour later is how the last one had to be done.
pub async fn download_collaborative_archive(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Response, AppError> {
    let archive = crate::web::handlers::collaborate::archive::download_session(&state, room_uuid)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to read the archive: {}",
                crate::web::handlers::collaborate::archive::describe(&*e)
            )
        })?;
    if archive.is_empty() {
        return Err(AppError::NotFound(
            "No archive for this session".to_string(),
        ));
    }

    // A recording is mostly its own framing and goes over the wire at about a
    // third of its size. Compressed here rather than passed through from
    // storage: a session is kept as one gzip member per chunk, and
    // concatenating members is legal but leans on every consumer handling a
    // multi-member stream. Since this path already has to inflate each chunk
    // to join them, one deflate on the way out removes the question, and the
    // measured difference between one member and several is nothing.
    //
    // Content-Encoding rather than a `.gz` body, so what a reader decodes is
    // the recording either way -- the browser inflates before the player sees
    // it, and a person following the link gets the plain file.
    let wants_gzip = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("gzip"));

    let compressed = if wants_gzip {
        // Off the runtime: this is the whole session in one go, and a large
        // one is hundreds of milliseconds of CPU.
        match crate::web::handlers::collaborate::archive::compress_off_thread(archive.clone()).await
        {
            Ok(compressed) => Some(compressed),
            // Not worth failing a download over; it is only smaller.
            Err(e) => {
                tracing::warn!("Could not compress the archive for {}: {}", room_uuid, e);
                None
            }
        }
    } else {
        None
    };

    let mut response = axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{room_uuid}.oeeelog\""),
        );
    if compressed.is_some() {
        response = response.header(header::CONTENT_ENCODING, "gzip");
    }
    Ok(response
        .body(axum::body::Body::from(compressed.unwrap_or(archive)))
        .map_err(|e| anyhow::anyhow!("Failed to build the archive response: {}", e))?)
}

/// Refuses, by name, to answer from a deployment that records nothing.
///
/// Without this a list of what was kept comes back empty, which reads as
/// "nobody said anything" or "nobody filed a report" when the truth is that
/// there was never anywhere to keep either.
fn require_recording(state: &AppState) -> Result<(), AppError> {
    match crate::web::handlers::collaborate::archive::bucket(&state.config) {
        Some(_) => Ok(()),
        // Worded for the " not found" `NotFound` appends.
        None => Err(AppError::NotFound(
            "Recording storage (archive_s3_bucket is unset on this deployment)".to_string(),
        )),
    }
}

/// GET /admin/collaborative-sessions/:uuid/diagnostics — what each client
/// believed about its own position, for the moments one of them said
/// something was wrong.
///
/// Staff only, like the recording it belongs to: a report carries one
/// person's session in enough detail to reconstruct it.
pub async fn download_collaborative_diagnostics(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    require_recording(&state)?;
    let reports =
        crate::web::handlers::collaborate::archive::download_diagnostics(&state, room_uuid)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read the reports: {}",
                    crate::web::handlers::collaborate::archive::describe(&*e)
                )
            })?;
    Ok(axum::Json(reports).into_response())
}

/// GET /admin/collaborative-sessions/:uuid/manifest — what a recording says
/// about itself, for the player to size a canvas and name the layers by.
pub async fn collaborative_archive_manifest(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let manifest = crate::web::handlers::collaborate::archive::read_manifest(&state, room_uuid)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to read the manifest: {}",
                crate::web::handlers::collaborate::archive::describe(&*e)
            )
        })?;
    match manifest {
        Some(manifest) => Ok(axum::Json(manifest).into_response()),
        None => Err(AppError::NotFound(
            "No recording for this session".to_string(),
        )),
    }
}

/// GET /admin/collaborative-sessions/:uuid/chat — what was said in a session,
/// beside the recording of what was drawn.
///
/// Chat never enters canonical history: it is broadcast and forgotten, with
/// only the last hundred lines held for somebody joining. This is the kept
/// copy, and staff-only like everything else about a recording -- a
/// transcript is the most personal thing a session produces.
pub async fn collaborative_session_chat(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    require_recording(&state)?;
    let lines = crate::web::handlers::collaborate::archive::read_chat(&state, room_uuid)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to read the transcript: {}",
                crate::web::handlers::collaborate::archive::describe(&*e)
            )
        })?;
    Ok(axum::Json(lines).into_response())
}

#[derive(Debug, Deserialize)]
pub struct TailQuery {
    #[serde(default)]
    after: u64,
}

/// GET /admin/collaborative-sessions/:uuid/archive/tail?after=N — what has been
/// recorded since sequence N, for following a live session without flushing
/// it on every look.
pub async fn collaborative_archive_tail(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    Query(query): Query<TailQuery>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    require_recording(&state)?;
    let tail =
        crate::web::handlers::collaborate::archive::read_tail(&state, room_uuid, query.after)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to read the recording: {}",
                    crate::web::handlers::collaborate::archive::describe(&*e)
                )
            })?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], tail).into_response())
}

/// Who holds which session id right now, for naming the marks of somebody
/// who joined after the manifest was last written.
#[derive(serde::Serialize)]
struct Seat {
    session_id: u8,
    login_name: String,
}

#[derive(serde::Serialize)]
struct SessionDetails {
    session: crate::models::admin::AdminCollaborativeSession,
    participants: Vec<crate::models::admin::AdminSessionParticipant>,
    seats: Vec<Seat>,
}

/// GET /admin/collaborative-sessions/:uuid/details — what the database knows
/// about a session, for the inspector's header: the recording itself does not
/// carry a title, an owner or a community.
pub async fn collaborative_session_details(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let session = crate::models::admin::find_collaborative_session(&mut tx, room_uuid)
        .await?
        .ok_or_else(|| AppError::NotFound("Session".to_string()))?;
    let participants =
        crate::models::admin::find_collaborative_session_participants(&mut tx, room_uuid).await?;
    tx.commit().await?;

    // The live assignment expires with the room; the manifest's copy is the
    // record after that, and an empty list here says only that it has gone.
    let assigned = state
        .redis_state
        .get_user_ids(room_uuid)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("Could not read the seats for {}: {}", room_uuid, e);
            Default::default()
        });
    let mut seats: Vec<Seat> = participants
        .iter()
        .filter_map(|participant| {
            assigned.get(&participant.user_id).map(|session_id| Seat {
                session_id: *session_id,
                login_name: participant.login_name.clone(),
            })
        })
        .collect();
    seats.sort_by_key(|seat| seat.session_id);

    Ok(axum::Json(SessionDetails {
        session,
        participants,
        seats,
    })
    .into_response())
}

/// GET /admin/collaborative-sessions/:uuid/reference — the image the session
/// was saved as, for the inspector to compare its replay against.
///
/// Passed through from storage rather than linked: the public image host is
/// another origin, and a canvas that draws a cross-origin image cannot have
/// its pixels read back, which is the whole of the comparison.
pub async fn collaborative_session_reference(
    _admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let filename = sqlx::query_scalar!(
        r#"
        SELECT i.image_filename
        FROM collaborative_sessions cs
        JOIN posts p ON p.id = cs.saved_post_id
        JOIN images i ON i.id = p.image_id
        WHERE cs.id = $1
        "#,
        room_uuid,
    )
    .fetch_optional(&state.db_pool)
    .await?
    .ok_or_else(|| AppError::NotFound("A saved post for this session".to_string()))?;
    // Where the image store puts a post's picture; see save_session_to_post.
    let key = format!("image/{}/{}", filename.get(..2).unwrap_or(""), filename);
    let object = crate::web::handlers::collaborate::archive::s3_client(&state.config)
        .get_object()
        .bucket(&state.config.aws_s3_bucket)
        .key(&key)
        .send()
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to read {}: {}",
                key,
                crate::web::handlers::collaborate::archive::describe(&e)
            )
        })?;
    let bytes = object
        .body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", key, e))?
        .into_bytes();
    Ok(([(header::CONTENT_TYPE, "image/png")], bytes).into_response())
}

/// POST /admin/collaborative-sessions/:uuid/check — what the inspector found
/// when it played the recording back, kept for the session list.
pub async fn record_collaborative_session_check(
    admin: AdminUser,
    Path(room_uuid): Path<Uuid>,
    State(state): State<AppState>,
    axum::Json(check): axum::Json<crate::models::collaborative_recording::ReplayCheck>,
) -> Result<Response, AppError> {
    if !["match", "differs", "incomplete", "unavailable"].contains(&check.outcome.as_str()) {
        return Err(AppError::BadRequest(format!(
            "unknown check outcome {:?}",
            check.outcome
        )));
    }
    crate::models::collaborative_recording::note_check(
        &state.db_pool,
        room_uuid,
        admin.0.id,
        &check,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// GET /admin/collaborative-sessions/:uuid — the recording, played back
/// through the painter that drew it, with the log, the conversation and the
/// reports read against it.
///
/// The page holds no permission of its own: it fetches the manifest and the
/// log from the two admin endpoints above, so serving it to anyone else would
/// get them a viewer that can read nothing.
pub async fn replay_collaborative_session(
    _admin: AdminUser,
    Path(_room_uuid): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let html = std::fs::read_to_string("neo-cucumber/dist-replay/index.html")
        .map_err(|_| anyhow::anyhow!("The replay viewer has not been built"))?;
    let head = state.render("admin/replay_head.jinja", context! {}).await?;
    Ok(Html(with_head(&html, &head)).into_response())
}

/// `html` with `head` added at the end of its <head>, or unchanged if it has
/// none to add to.
pub(super) fn with_head(html: &str, head: &str) -> String {
    match html.find("</head>") {
        Some(at) => format!("{}{}{}", &html[..at], head, &html[at..]),
        None => html.to_string(),
    }
}
