//! What a live collaborative canvas currently looks like, for the lobby.
//!
//! The server cannot draw. Room state is a log of NEO drawing operations plus
//! per-layer checkpoint snapshots, and turning that into a picture means the
//! engine in `neo-cucumber` -- a second implementation here would be a second
//! answer to "what does this stroke look like", which is the one divergence
//! this codebase cannot afford. So a participant's browser, which has the
//! canvas composited already, renders the preview and uploads it, exactly as
//! it already does for the finished drawing in `save_collaborative_session`.
//!
//! Which participant does the work is decided the way a checkpoint volunteer
//! is (see `RedisStateManager::claim_reset_upload`): whoever asks first inside
//! an open window gets a token, and the window is what paces the refresh. The
//! token comes back on the upload, so a client that claimed, went away, and
//! returned after its window closed cannot overwrite a newer preview with the
//! canvas it had a minute ago.
//!
//! The bytes are user-supplied and served to anyone who can see the session,
//! which is what separates this from the checkpoint path: a snapshot only ever
//! goes back to the room, whereas a preview goes into an `<img>` on a public
//! page. `inspect_image` is therefore not a formality -- it is the only thing
//! standing between a participant and a decompression bomb on the lobby.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use redis::AsyncCommands;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::user::AuthSession;
use crate::redis::RedisPool;
use crate::web::state::AppState;

use super::redis_state::ACTIVITY_TTL;

/// The longest edge a preview may have.
///
/// A lobby card is 300px wide, so this is already generous; it exists to bound
/// what a browser is asked to decode, not to look good at full size.
pub const MAX_PREVIEW_EDGE: u32 = 400;

/// The largest upload accepted, before which the request body is not even read
/// to the end.
///
/// WEBP at this size lands in the low tens of KiB. The headroom is for the PNG
/// a browser without a WEBP encoder falls back to, silently, from the same
/// `toBlob` call.
pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

/// How long one client holds the right to upload, and so how often a room's
/// preview can change.
///
/// The claim is not cleared on a successful upload: letting it expire is what
/// spaces uploads out, and it means a claimant that goes quiet costs the room
/// one ordinary window rather than a special case.
const PREVIEW_CLAIM_TTL: u64 = 30;

/// Kept for as long as the room's other Redis state, and expiring with it: a
/// preview of a session nobody has touched in an hour is not worth serving.
const PREVIEW_TTL: u64 = ACTIVITY_TTL;

const PREVIEW_PREFIX: &str = "oeee:preview:v1:";
const PREVIEW_META_PREFIX: &str = "oeee:preview_meta:v1:";
const PREVIEW_CLAIM_PREFIX: &str = "oeee:preview_claim:v1:";

/// The value the claim key holds once its window has been spent, so the token
/// that spent it cannot be presented twice.
const CLAIM_CONSUMED: &str = "-";

fn preview_key(room_uuid: Uuid) -> String {
    format!("{}{}", PREVIEW_PREFIX, room_uuid)
}

fn preview_meta_key(room_uuid: Uuid) -> String {
    format!("{}{}", PREVIEW_META_PREFIX, room_uuid)
}

fn preview_claim_key(room_uuid: Uuid) -> String {
    format!("{}{}", PREVIEW_CLAIM_PREFIX, room_uuid)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// The image formats a browser can hand us from `canvas.toBlob`.
///
/// WEBP is what is asked for; PNG is what arrives instead on a browser whose
/// encoder does not know WEBP, because `toBlob` falls back to it without
/// saying so. Accepting both is not generosity -- rejecting PNG would drop
/// previews from those browsers with no diagnosis anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Webp,
}

impl ImageKind {
    pub fn content_type(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Webp => "image/webp",
        }
    }

    fn from_content_type(value: &str) -> Option<Self> {
        // Split on ';' so "image/png; charset=..." from an over-helpful client
        // is not a rejection.
        match value.split(';').next()?.trim() {
            "image/png" => Some(ImageKind::Png),
            "image/webp" => Some(ImageKind::Webp),
            _ => None,
        }
    }
}

/// The dimensions a preview of this canvas must have.
///
/// Computed here and handed to the browser on the claim, rather than worked
/// out at both ends: the upload is rejected unless it matches exactly, and two
/// implementations of one rounding rule is how that turns into a browser that
/// silently stops being able to upload. Having an expected size at all is what
/// makes the check possible -- without it a 16000x16000 image that decompresses
/// to a gigabyte is a valid preview of a 300x300 canvas.
pub fn preview_dimensions(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest == 0 {
        return (1, 1);
    }
    if longest <= MAX_PREVIEW_EDGE {
        return (width.max(1), height.max(1));
    }
    // Round half up, in integers, so this cannot disagree with the browser's
    // Math.round over a fraction neither of them can represent.
    let scaled = |edge: u32| -> u32 {
        let numerator = edge as u64 * MAX_PREVIEW_EDGE as u64 * 2 + longest as u64;
        ((numerator / (longest as u64 * 2)) as u32).max(1)
    };
    (scaled(width), scaled(height))
}

/// The format and dimensions of an image, from its header alone.
///
/// None for anything this does not positively recognise, which is the point:
/// what reaches here is bytes a participant chose, and everything downstream
/// treats the result as trustworthy.
pub fn inspect_image(bytes: &[u8]) -> Option<(ImageKind, u32, u32)> {
    inspect_png(bytes)
        .map(|(w, h)| (ImageKind::Png, w, h))
        .or_else(|| inspect_webp(bytes).map(|(w, h)| (ImageKind::Webp, w, h)))
}

fn read_u32_be(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// PNG: an 8-byte signature, then IHDR, whose first two fields are the size.
fn inspect_png(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some((read_u32_be(&bytes[16..20]), read_u32_be(&bytes[20..24])))
}

/// WEBP: a RIFF container whose first chunk carries the size, in one of three
/// encodings depending on which VP8 flavour the encoder chose.
fn inspect_webp(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let chunk = &bytes[12..16];
    let payload = bytes.get(20..)?;
    match chunk {
        // Lossy. Three bytes of frame tag, a three-byte sync code, then 14-bit
        // width and height with two scaling bits above each.
        b"VP8 " => {
            if payload.len() < 10 || payload[3..6] != [0x9D, 0x01, 0x2A] {
                return None;
            }
            let width = read_u16_le(&payload[6..8]) & 0x3FFF;
            let height = read_u16_le(&payload[8..10]) & 0x3FFF;
            Some((width as u32, height as u32))
        }
        // Lossless. A signature byte, then two 14-bit fields packed
        // little-endian, each stored one less than the real edge.
        b"VP8L" => {
            if payload.len() < 5 || payload[0] != 0x2F {
                return None;
            }
            let packed = read_u32_le(&payload[1..5]);
            let width = (packed & 0x3FFF) + 1;
            let height = ((packed >> 14) & 0x3FFF) + 1;
            Some((width, height))
        }
        // Extended -- what an encoder emits when the image has alpha or
        // metadata. Four bytes of flags, then the canvas size as two 24-bit
        // fields, again stored one less than the real edge.
        b"VP8X" => {
            if payload.len() < 10 {
                return None;
            }
            let edge = |at: usize| -> u32 {
                u32::from_le_bytes([payload[at], payload[at + 1], payload[at + 2], 0]) + 1
            };
            Some((edge(4), edge(7)))
        }
        _ => None,
    }
}

/// A preview and what is needed to serve it without decoding it again.
pub struct StoredPreview {
    pub bytes: Vec<u8>,
    pub kind: ImageKind,
    /// Milliseconds since the epoch, doubling as the cache-busting `v` on the
    /// lobby's `<img>` and as the ETag here.
    pub version: u64,
}

/// Previews in Redis, alongside the room state they expire with.
#[derive(Clone)]
pub struct PreviewStore {
    pool: RedisPool,
}

type StoreResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

impl PreviewStore {
    pub fn new(pool: RedisPool) -> Self {
        Self { pool }
    }

    /// Opens a window if none is open, and hands the caller the only token
    /// that will be accepted for it. None means somebody else has it, or the
    /// last one has not expired yet -- either way, don't render.
    pub async fn claim(&self, room_uuid: Uuid) -> StoreResult<Option<String>> {
        let mut conn = self.pool.get().await?;
        let token = Uuid::new_v4().to_string();
        let acquired = redis::cmd("SET")
            .arg(preview_claim_key(room_uuid))
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(PREVIEW_CLAIM_TTL)
            .query_async::<Option<String>>(&mut *conn)
            .await?
            .is_some();
        Ok(acquired.then_some(token))
    }

    /// Stores a preview against a token, and spends the window in the same
    /// step so the token cannot be presented again.
    ///
    /// KEEPTTL on the claim rather than a fresh expiry: the window paces
    /// uploads from when it opened, and an upload that arrives late in one
    /// should not push the next one a full window further out.
    pub async fn store(
        &self,
        room_uuid: Uuid,
        token: &str,
        kind: ImageKind,
        bytes: &[u8],
    ) -> StoreResult<Option<u64>> {
        const STORE_SCRIPT: &str = r#"
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
    return 0
end
redis.call('SET', KEYS[1], ARGV[2], 'KEEPTTL')
redis.call('SET', KEYS[2], ARGV[3], 'EX', tonumber(ARGV[5]))
redis.call('SET', KEYS[3], ARGV[4], 'EX', tonumber(ARGV[5]))
return 1
"#;
        let version = now_millis();
        let mut conn = self.pool.get().await?;
        let stored: i64 = redis::Script::new(STORE_SCRIPT)
            .key(preview_claim_key(room_uuid))
            .key(preview_key(room_uuid))
            .key(preview_meta_key(room_uuid))
            .arg(token)
            .arg(CLAIM_CONSUMED)
            .arg(bytes)
            .arg(format!("{}:{}", version, kind.content_type()))
            .arg(PREVIEW_TTL)
            .invoke_async(&mut *conn)
            .await?;
        Ok((stored == 1).then_some(version))
    }

    /// The version of each room's preview, in the order asked for, for a lobby
    /// deciding which cards have a picture to show. None where there is none.
    ///
    /// One round trip for the whole page: the alternative is a card that
    /// always emits an `<img>` and a broken-image icon wherever no participant
    /// has uploaded yet.
    pub async fn versions(&self, room_uuids: &[Uuid]) -> StoreResult<Vec<Option<u64>>> {
        if room_uuids.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.pool.get().await?;
        let keys: Vec<String> = room_uuids.iter().map(|id| preview_meta_key(*id)).collect();
        let metas: Vec<Option<String>> = conn.mget(&keys).await?;
        Ok(metas
            .into_iter()
            .map(|meta| meta.and_then(|meta| parse_meta(&meta).map(|(version, _)| version)))
            .collect())
    }

    /// The stored version alone, for answering a conditional request without
    /// moving the image.
    pub async fn version(&self, room_uuid: Uuid) -> StoreResult<Option<u64>> {
        let mut conn = self.pool.get().await?;
        let meta: Option<String> = conn.get(preview_meta_key(room_uuid)).await?;
        Ok(meta.and_then(|meta| parse_meta(&meta).map(|(version, _)| version)))
    }

    pub async fn load(&self, room_uuid: Uuid) -> StoreResult<Option<StoredPreview>> {
        let mut conn = self.pool.get().await?;
        let (bytes, meta): (Option<Vec<u8>>, Option<String>) = conn
            .mget(&[preview_key(room_uuid), preview_meta_key(room_uuid)])
            .await?;
        let (Some(bytes), Some(meta)) = (bytes, meta) else {
            return Ok(None);
        };
        let Some((version, content_type)) = parse_meta(&meta) else {
            return Ok(None);
        };
        let Some(kind) = ImageKind::from_content_type(&content_type) else {
            return Ok(None);
        };
        Ok(Some(StoredPreview {
            bytes,
            kind,
            version,
        }))
    }

    /// Drops a room's preview and any open window, for a session that has
    /// ended.
    pub async fn cleanup(&self, room_uuid: Uuid) -> StoreResult<()> {
        let mut conn = self.pool.get().await?;
        conn.del::<_, ()>(&[
            preview_key(room_uuid),
            preview_meta_key(room_uuid),
            preview_claim_key(room_uuid),
        ])
        .await?;
        Ok(())
    }
}

fn parse_meta(meta: &str) -> Option<(u64, String)> {
    let (version, content_type) = meta.split_once(':')?;
    Some((version.parse().ok()?, content_type.to_string()))
}

/// Preview versions for a batch of rooms, in the order asked for, for any page
/// that lists sessions.
///
/// Never fails: a missing preview is ordinary -- a session nobody has drawn in
/// has none -- and a Redis that is unwell should cost a page its pictures
/// rather than the page.
pub async fn preview_versions(state: &AppState, room_uuids: &[Uuid]) -> Vec<Option<u64>> {
    if room_uuids.is_empty() {
        return Vec::new();
    }
    let store = PreviewStore::new(state.redis_pool.clone());
    match store.versions(room_uuids).await {
        Ok(versions) => versions,
        Err(e) => {
            warn!("Failed to read preview versions: {}", e);
            vec![None; room_uuids.len()]
        }
    }
}

/// Whether this viewer is allowed to see the session's canvas at all.
///
/// The same rule the lobby lists by, because the lobby is what displays this:
/// a public session in a public place is public, and anything else is for the
/// people already in it. A session that has ended has no live canvas to show.
///
/// Except for staff, who get the same exemption here that every other admin
/// listing has: `/admin/collaborative-sessions` exists to show the rooms
/// nobody else can see, and a page of them with the pictures missing would
/// show the least about exactly the sessions it is there for.
async fn viewer_may_watch(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
    viewer_user_id: Option<Uuid>,
    viewer_is_admin: bool,
) -> Result<bool, sqlx::Error> {
    if viewer_is_admin {
        return session_is_live(db, room_uuid).await;
    }
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM collaborative_sessions cs
            LEFT JOIN communities c ON cs.community_id = c.id
            WHERE cs.id = $1
              AND cs.ended_at IS NULL
              AND (
                (cs.is_public AND (cs.community_id IS NULL OR c.visibility = 'public'))
                OR cs.owner_id = $2
                OR EXISTS(
                    SELECT 1 FROM collaborative_sessions_participants csp
                    WHERE csp.session_id = cs.id AND csp.user_id = $2
                )
              )
        ) AS "exists!"
        "#,
        room_uuid,
        viewer_user_id,
    )
    .fetch_one(db)
    .await
}

/// A session with a canvas still in it. What an admin's exemption is an
/// exemption *from* is the visibility rule, not from the session being over:
/// an ended room's Redis state is deleted with it, so there is nothing to
/// serve either way.
async fn session_is_live(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM collaborative_sessions
            WHERE id = $1 AND ended_at IS NULL
        ) AS "exists!"
        "#,
        room_uuid,
    )
    .fetch_one(db)
    .await
}

/// Who may upload one: somebody actually drawing in the room.
///
/// Not "may watch" -- a preview is a claim about what the canvas looks like,
/// and only a client with the canvas can make it honestly.
/// Whether this viewer has ever drawn in this room, active or not.
///
/// Looser than `viewer_may_upload` on purpose: a client filing a report about
/// a session it has just fallen out of is exactly the report worth having, and
/// by then its participant row is no longer active.
pub async fn viewer_is_participant(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM collaborative_sessions cs
            WHERE cs.id = $1
              AND (
                cs.owner_id = $2
                OR EXISTS(
                    SELECT 1 FROM collaborative_sessions_participants csp
                    WHERE csp.session_id = cs.id AND csp.user_id = $2
                )
              )
        ) AS "exists!"
        "#,
        room_uuid,
        user_id,
    )
    .fetch_one(db)
    .await
}

async fn viewer_may_upload(
    db: &sqlx::Pool<sqlx::Postgres>,
    room_uuid: Uuid,
    user_id: Uuid,
) -> Result<Option<(u32, u32)>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT cs.width, cs.height
        FROM collaborative_sessions cs
        WHERE cs.id = $1
          AND cs.ended_at IS NULL
          AND EXISTS(
            SELECT 1 FROM collaborative_sessions_participants csp
            WHERE csp.session_id = cs.id
              AND csp.user_id = $2
              AND csp.is_active = true
          )
        "#,
        room_uuid,
        user_id,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| (row.width.max(0) as u32, row.height.max(0) as u32)))
}

#[derive(Serialize)]
pub struct PreviewClaimResponse {
    pub token: String,
    /// What to scale to, and what the upload is then checked against. Sent
    /// rather than left for the browser to work out, so there is one rounding
    /// rule in existence instead of two that have to agree forever.
    pub width: u32,
    pub height: u32,
    /// The budget the encoder has to come in under. The browser steps its
    /// quality down until it fits and gives up rather than uploading something
    /// that would be refused here.
    pub max_bytes: usize,
}

/// POST /collaborate/:uuid/preview/claim -- ask to be the one who uploads.
///
/// Cheap on purpose, and separate from the upload for that reason: rendering
/// and encoding a canvas is the expensive part, and every client that is not
/// going to win should find out before doing it, not after.
pub async fn claim_session_preview(
    Path(room_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    let Some((width, height)) = viewer_may_upload(&state.db_pool, room_uuid, user.id).await? else {
        return Err(AppError::Forbidden);
    };

    let store = PreviewStore::new(state.redis_pool.clone());
    let Some(token) = store
        .claim(room_uuid)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to claim a preview window: {}", e))?
    else {
        // Somebody else is refreshing this room, or the last refresh is still
        // recent. Both mean "not you, not now", which is the whole answer.
        return Ok(StatusCode::CONFLICT.into_response());
    };

    let (preview_width, preview_height) = preview_dimensions(width, height);
    Ok(Json(PreviewClaimResponse {
        token,
        width: preview_width,
        height: preview_height,
        max_bytes: MAX_PREVIEW_BYTES,
    })
    .into_response())
}

/// PUT /collaborate/:uuid/preview -- deliver the image claimed for.
pub async fn upload_session_preview(
    Path(room_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    let Some((width, height)) = viewer_may_upload(&state.db_pool, room_uuid, user.id).await? else {
        return Err(AppError::Forbidden);
    };

    let Some(token) = headers
        .get("x-preview-token")
        .and_then(|value| value.to_str().ok())
        .filter(|token| !token.is_empty())
    else {
        return Err(AppError::InvalidFormData(
            "missing preview token".to_string(),
        ));
    };

    if body.len() > MAX_PREVIEW_BYTES {
        return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
    }

    let Some((kind, image_width, image_height)) = inspect_image(&body) else {
        return Err(AppError::InvalidFormData(
            "preview is not a PNG or WEBP image".to_string(),
        ));
    };

    // The declared Content-Type is not what is believed -- the header above is
    // -- but a mismatch means the client is confused about what it encoded,
    // and a preview served under the wrong type will not render.
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(ImageKind::from_content_type)
        != Some(kind)
    {
        return Err(AppError::InvalidFormData(
            "preview content type does not match its contents".to_string(),
        ));
    }

    let expected = preview_dimensions(width, height);
    if (image_width, image_height) != expected {
        warn!(
            "Rejected a {}x{} preview for room {} ({}x{} expected)",
            image_width, image_height, room_uuid, expected.0, expected.1
        );
        return Err(AppError::InvalidFormData(format!(
            "preview must be {}x{}",
            expected.0, expected.1
        )));
    }

    let store = PreviewStore::new(state.redis_pool.clone());
    let stored = store
        .store(room_uuid, token, kind, &body)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to store a preview: {}", e))?;

    match stored {
        Some(version) => {
            debug!(
                "Stored a {} byte preview for room {} at version {}",
                body.len(),
                room_uuid,
                version
            );
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        // The window closed while this was being rendered, or was already
        // spent. Somebody else's preview is the current one and this canvas is
        // the older news.
        None => Ok(StatusCode::CONFLICT.into_response()),
    }
}

/// POST /collaborate/:uuid/diagnostics -- one client's account of where it
/// thought it was.
///
/// The counterpart to the canonical archive, and the half that catches what
/// the archive cannot: a stream can be recorded perfectly and still be applied
/// wrongly, which is what happened to session c9b8321d. Filed by the client on
/// trouble, so a healthy session posts nothing.
/// The shortest gap between two reports from one participant of one session.
const DIAGNOSTIC_INTERVAL_SECS: u64 = 30;
/// The most reports kept for one session. A report is filed when a canvas
/// goes wrong, and a session that goes wrong this often has said everything
/// it can.
const MAX_DIAGNOSTICS_PER_SESSION: i64 = 50;

pub async fn report_session_diagnostics(
    Path(room_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, AppError> {
    let user = auth_session.user.ok_or(AppError::Unauthorized)?;

    if !viewer_is_participant(&state.db_pool, room_uuid, user.id).await? {
        return Err(AppError::Forbidden);
    }
    if body.len() > super::archive::MAX_DIAGNOSTIC_BYTES {
        return Ok(StatusCode::PAYLOAD_TOO_LARGE.into_response());
    }
    // A participant may file a report whenever their canvas goes wrong, for
    // as long as the session row exists, and each one is an object of up to
    // half a megabyte in the private bucket. Bounded per person and per
    // session, so a loop cannot fill the bucket or bury the genuine reports
    // under junk with the same prefix.
    let mut redis = state.redis_pool.get().await?;
    let admitted: Option<String> = redis::cmd("SET")
        .arg(format!("oeee:diagnostic_rate:{room_uuid}:{}", user.id))
        .arg("1")
        .arg("NX")
        .arg("EX")
        .arg(DIAGNOSTIC_INTERVAL_SECS)
        .query_async(&mut *redis)
        .await?;
    if admitted.is_none() {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    if crate::models::collaborative_recording::report_count(&state.db_pool, room_uuid).await?
        >= MAX_DIAGNOSTICS_PER_SESSION
    {
        warn!(
            "Dropping a report from {} for room {}: already holds {}",
            user.login_name, room_uuid, MAX_DIAGNOSTICS_PER_SESSION
        );
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    // Stored as sent, but only once it is known to be the shape it claims:
    // this is written to object storage under an admin-readable prefix, and
    // "whatever the client posted" is not a thing to keep there.
    if serde_json::from_slice::<serde_json::Value>(&body).is_err() {
        return Err(AppError::InvalidFormData(
            "diagnostic report is not JSON".to_string(),
        ));
    }

    match super::archive::store_diagnostic(&state, room_uuid, &user.login_name, body.to_vec()).await
    {
        Ok(Some(key)) => {
            warn!(
                "Stored a synchronization report from {} for room {} at {}",
                user.login_name, room_uuid, key
            );
            // Counted where the admin list can see it, which is what makes a
            // reported session stand out there.
            if let Err(e) =
                crate::models::collaborative_recording::note_report(&state.db_pool, room_uuid).await
            {
                warn!("Failed to count the report for room {}: {}", room_uuid, e);
            }
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        // Nowhere private to keep it. Accepted rather than refused: the client
        // has nothing to do differently, and a report is not worth an error on
        // a path that is already going wrong.
        Ok(None) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => Err(anyhow::anyhow!("Failed to store a diagnostic report: {}", e).into()),
    }
}

/// GET /collaborate/:uuid/preview -- the image itself.
///
/// The lobby asks for it with the version it rendered as `?v=`, so a card that
/// has been swapped in with a new version misses the browser cache and
/// everything else hits it.
pub async fn serve_session_preview(
    Path(room_uuid): Path<Uuid>,
    auth_session: AuthSession,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let viewer_user_id = auth_session.user.as_ref().map(|user| user.id);
    let viewer_is_admin = auth_session
        .user
        .as_ref()
        .is_some_and(|user| user.is_admin());
    if !viewer_may_watch(&state.db_pool, room_uuid, viewer_user_id, viewer_is_admin).await? {
        return Err(AppError::NotFound("Session not found".to_string()));
    }

    let store = PreviewStore::new(state.redis_pool.clone());
    let requested_etag = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string());

    if let Some(requested_etag) = requested_etag.as_deref() {
        let current = store
            .version(room_uuid)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read a preview version: {}", e))?;
        if current.map(etag_for) == Some(requested_etag.to_string()) {
            return Ok(not_modified(requested_etag.to_string()));
        }
    }

    let Some(preview) = store
        .load(room_uuid)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read a preview: {}", e))?
    else {
        return Err(AppError::NotFound("No preview yet".to_string()));
    };

    Ok((
        [
            (
                header::CONTENT_TYPE,
                preview.kind.content_type().to_string(),
            ),
            (header::ETAG, etag_for(preview.version)),
            // Private: a link-only session's canvas must not be held by a
            // shared cache that never saw the check above. no-cache, not
            // no-store: the browser may keep it, it just has to ask.
            (
                header::CACHE_CONTROL,
                "private, no-cache, max-age=0".to_string(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        preview.bytes,
    )
        .into_response())
}

fn etag_for(version: u64) -> String {
    format!("\"{}\"", version)
}

fn not_modified(etag: String) -> Response {
    (
        StatusCode::NOT_MODIFIED,
        [
            (header::ETAG, etag),
            (
                header::CACHE_CONTROL,
                "private, no-cache, max-age=0".to_string(),
            ),
        ],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canvas_within_the_box_is_previewed_at_its_own_size() {
        assert_eq!(preview_dimensions(300, 300), (300, 300));
    }

    /// The other canvas the lobby offers. Both edges scale by one factor, so
    /// the card never shows a stretched drawing.
    #[test]
    fn a_wider_canvas_keeps_its_aspect_ratio() {
        assert_eq!(preview_dimensions(1024, 768), (400, 300));
    }

    #[test]
    fn every_offered_canvas_size_previews_within_the_box() {
        for (width, height) in super::super::http_handlers::CANVAS_SIZES {
            let (preview_width, preview_height) = preview_dimensions(width, height);
            assert!(
                preview_width <= MAX_PREVIEW_EDGE && preview_height <= MAX_PREVIEW_EDGE,
                "{width}x{height} previews as {preview_width}x{preview_height}"
            );
            assert!(preview_width > 0 && preview_height > 0);
        }
    }

    /// A canvas thinner than the rounding step still gets a real image rather
    /// than a zero-width one, which no encoder will produce.
    #[test]
    fn a_sliver_of_a_canvas_still_has_both_edges() {
        assert_eq!(preview_dimensions(4000, 3), (400, 1));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn webp(chunk: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&((payload.len() + 12) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(chunk);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    fn lossy_webp(width: u16, height: u16) -> Vec<u8> {
        let mut payload = vec![0x00, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&[0; 8]);
        webp(b"VP8 ", payload)
    }

    fn lossless_webp(width: u32, height: u32) -> Vec<u8> {
        let packed = (width - 1) | ((height - 1) << 14);
        let mut payload = vec![0x2F];
        payload.extend_from_slice(&packed.to_le_bytes());
        payload.extend_from_slice(&[0; 8]);
        webp(b"VP8L", payload)
    }

    fn extended_webp(width: u32, height: u32) -> Vec<u8> {
        let mut payload = vec![0x10, 0x00, 0x00, 0x00];
        payload.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        payload.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        payload.extend_from_slice(&[0; 8]);
        webp(b"VP8X", payload)
    }

    #[test]
    fn reads_the_size_of_a_png() {
        assert_eq!(
            inspect_image(&png(400, 300)),
            Some((ImageKind::Png, 400, 300))
        );
    }

    /// The three ways a browser's WEBP encoder can describe the same canvas.
    /// Which one arrives is not ours to choose -- `toBlob` picks per image,
    /// and an unrecognised flavour is an upload rejected for no reason a
    /// participant could act on.
    #[test]
    fn reads_the_size_of_every_webp_flavour() {
        assert_eq!(
            inspect_image(&lossy_webp(400, 300)),
            Some((ImageKind::Webp, 400, 300))
        );
        assert_eq!(
            inspect_image(&lossless_webp(400, 300)),
            Some((ImageKind::Webp, 400, 300))
        );
        assert_eq!(
            inspect_image(&extended_webp(400, 300)),
            Some((ImageKind::Webp, 400, 300))
        );
    }

    /// The largest edge each encoding can express, where an off-by-one in the
    /// bit packing shows up.
    #[test]
    fn reads_a_size_at_the_top_of_each_encodings_range() {
        assert_eq!(
            inspect_image(&lossy_webp(0x3FFF, 0x3FFF)),
            Some((ImageKind::Webp, 16383, 16383))
        );
        assert_eq!(
            inspect_image(&lossless_webp(16384, 16384)),
            Some((ImageKind::Webp, 16384, 16384))
        );
        assert_eq!(
            inspect_image(&extended_webp(16777216, 16777216)),
            Some((ImageKind::Webp, 16777216, 16777216))
        );
    }

    /// Real output from the encoder that will actually be uploading these.
    ///
    /// The synthetic cases above check the three WEBP encodings against the
    /// specification; this checks the one Chromium picks. It picks VP8X --
    /// the extended form, even for an opaque lossy image, where the size is
    /// two 24-bit fields rather than the 14-bit ones the plain forms use. A
    /// parser that handled only those would have read every real preview as
    /// the wrong size and rejected all of them, with nothing failing here to
    /// say so.
    ///
    /// Regenerate with a headless Chromium rendering a canvas of this size to
    /// `toBlob`, if either fixture ever needs replacing.
    #[test]
    fn reads_what_a_real_browser_encoder_produces() {
        let webp = include_bytes!("testdata/chromium-preview-400x300.webp");
        assert_eq!(inspect_image(webp), Some((ImageKind::Webp, 400, 300)));
        assert_eq!(&webp[12..16], b"VP8X");

        // The other format the same call can return, from the same encoder.
        let png = include_bytes!("testdata/chromium-preview-300x300.png");
        assert_eq!(inspect_image(png), Some((ImageKind::Png, 300, 300)));
    }

    /// What the browser encodes and what the server expects have to be the
    /// same number, or every upload is rejected for a size nobody chose.
    #[test]
    fn a_real_encode_matches_the_size_the_server_asks_for() {
        let webp = include_bytes!("testdata/chromium-preview-400x300.webp");
        let (_, width, height) = inspect_image(webp).expect("a webp");
        assert_eq!(preview_dimensions(1024, 768), (width, height));
    }

    #[test]
    fn rejects_bytes_that_are_not_an_image_we_serve() {
        assert_eq!(inspect_image(b""), None);
        assert_eq!(
            inspect_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            None
        );
        assert_eq!(inspect_image(&[0xFF, 0xD8, 0xFF, 0xE0]), None);
        // A GIF is an image, and is still not one of the two we accept.
        assert_eq!(inspect_image(b"GIF89a\x90\x01\x2c\x01"), None);
    }

    /// A truncated header must not be read past its end, and must not be
    /// believed either.
    #[test]
    fn rejects_a_header_that_stops_early() {
        for length in 0..24 {
            assert_eq!(inspect_image(&png(400, 300)[..length]), None, "{length}");
        }
        let lossy = lossy_webp(400, 300);
        for length in 0..20 {
            assert_eq!(inspect_image(&lossy[..length]), None, "{length}");
        }
    }

    /// RIFF is a container format shared with WAV and others; only the WEBP
    /// form of it is an image.
    #[test]
    fn rejects_a_riff_container_that_is_not_a_webp() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&64u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&[0; 32]);
        assert_eq!(inspect_image(&bytes), None);
    }

    #[test]
    fn reads_a_content_type_with_parameters() {
        assert_eq!(
            ImageKind::from_content_type("image/webp; charset=binary"),
            Some(ImageKind::Webp)
        );
        assert_eq!(ImageKind::from_content_type("image/gif"), None);
    }

    #[test]
    fn meta_round_trips_through_redis_encoding() {
        let meta = format!(
            "{}:{}",
            1_700_000_000_000u64,
            ImageKind::Webp.content_type()
        );
        assert_eq!(
            parse_meta(&meta),
            Some((1_700_000_000_000, "image/webp".to_string()))
        );
        assert_eq!(parse_meta("not-a-version:image/webp"), None);
        assert_eq!(parse_meta("123"), None);
    }
}
