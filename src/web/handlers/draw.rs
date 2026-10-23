use crate::app_error::{error_codes, AppError};
use crate::image_store;
use crate::models::banner::{create_banner, BannerDraft};
use crate::models::community::choice::{choose_communities, ChoiceQuery, ChoiceRule};
use crate::models::community::{
    find_community_by_id, is_user_member, Community, CommunityVisibility,
};
use crate::models::post::{
    create_post, find_continuable_draft, find_post_by_id, find_post_id_by_client_draft_id,
    replace_draft_drawing, DraftReplacement, PostDraft, Tool,
};
use crate::models::user::{AuthSession, User};
use crate::web::context::CommonContext;
use crate::web::handlers::{safe_decode_hash, safe_parse_uuid};
use crate::web::i18n::ExtractFtlLang;
use crate::web::presence::{Activity, Presence};
use crate::web::responses::ErrorResponse;
use crate::web::state::AppState;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::put_object::{PutObjectError, PutObjectOutput};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::Html,
    Form,
};
use chrono::Duration;
use data_encoding::BASE64;
use data_url::DataUrl;
use minijinja::context;
use minijinja::value::Serde;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha256::digest;
use sqlx::postgres::types::PgInterval;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Input {
    width: String,
    height: String,
    tool: Option<String>,
    community_id: Option<String>,
    parent_post_id: Option<String>,
    /// A drawing kept in this browser to carry on with (`LocalDraft.id`),
    /// from the drafts page. The painter finds it there itself; the server
    /// only passes it on, as it knows nothing of the browser's drafts.
    local_draft_id: Option<String>,
}

/// Whether `user` may draw for `community`, and so post what they drew there.
///
/// Signed in, it is what publishing already asks: a private community is for
/// its members, and anything else is open. A guest's drawing is kept on the
/// device until they have an account, and a guest can be a member of nothing,
/// so a guest may draw only for a public community -- an unlisted one is
/// reached by a link its members pass around, and a private one's name and
/// colours are not for a stranger's painter page.
async fn may_draw_in(
    tx: &mut Transaction<'_, Postgres>,
    user: Option<&User>,
    community: &Community,
) -> Result<bool, AppError> {
    may_draw_in_as(tx, user.map(|user| user.id), community).await
}

/// [`may_draw_in`], for whoever `user_id` is (None: somebody without an
/// account). A collaborative room in a community is opened by the same rule
/// (`collaborate::http_handlers::insert_session`), so the community page
/// offers both kinds of drawing to the same people.
pub(crate) async fn may_draw_in_as(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Option<uuid::Uuid>,
    community: &Community,
) -> Result<bool, AppError> {
    Ok(match (user_id, community.visibility) {
        (None, CommunityVisibility::Public) => true,
        (None, _) => false,
        (Some(user_id), CommunityVisibility::Private) => {
            is_user_member(tx, user_id, community.id).await?
        }
        (Some(_), _) => true,
    })
}

/// Where a guest is sent from a painter they may not open. Plain /login, with
/// no way back: the form that asked for the painter was a POST, which a
/// redirect cannot repeat.
fn sign_in_instead() -> Response {
    Redirect::to("/login").into_response()
}

/// What the post painter (`frontend/painter/entry.ts`) is told about the
/// drawing it opens, for `/draw` and for the relay page alike.
///
/// `userId` is what decides whether Save uploads: without it the painter
/// treats its reader as a guest and keeps the drawing on the device only. The
/// relay page once built this by hand and left it out, so a signed-in relay
/// was saved as a guest's drawing and never posted. One builder is what keeps
/// the two pages from telling the painter different things.
pub(crate) fn post_painter_config(
    width: u32,
    height: u32,
    community: Option<&Community>,
    parent_post_id: Option<&str>,
    locale: &str,
    user: Option<&User>,
) -> serde_json::Value {
    let mode = match community.and_then(|community| {
        Some((
            community.background_color.as_ref()?,
            community.foreground_color.as_ref()?,
        ))
    }) {
        Some((background_color, foreground_color)) => json!({
            "kind": "two-tone",
            "backgroundColor": background_color,
            "foregroundColor": foreground_color,
        }),
        None => json!({ "kind": "standard" }),
    };
    json!({
        "width": width,
        "height": height,
        "communityId": community.map(|c| c.id.to_string()),
        "communityName": community.map(|c| c.name.clone()),
        "parentPostId": parent_post_id,
        "locale": locale,
        "mode": mode,
        "userId": user.map(|u| u.id.to_string()),
    })
}

/// How many communities one answer of the New drawing search lists.
const COMMUNITY_CHOICES: i64 = 20;

/// `?q=`, what has been typed into the New drawing dialog's community search.
#[derive(Deserialize, Default)]
pub struct CommunitySearch {
    q: Option<String>,
}

/// GET /draw/communities: where the toolbar's New drawing may be drawn for
/// (`ChoiceRule::Draw`), for the dialog's search (draw_dialog.jinja). With
/// nothing typed, the reader's own; with something, any they may draw in
/// whose name, handle or description has it in, theirs first. A guest has
/// no own, and finds public ones. No community at all is the dialog's own
/// first choice: a personal post.
pub async fn draw_communities(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Query(search): Query<CommunitySearch>,
) -> Result<Response, AppError> {
    let reader = auth_session.user.as_ref().map(|user| user.id);
    let query = search.q.as_deref().unwrap_or("");
    let mut tx = state.db_pool.begin().await?;
    let found = choose_communities(
        &mut tx,
        &ChoiceQuery::new(ChoiceRule::Draw, reader, query, COMMUNITY_CHOICES),
    )
    .await?;
    tx.commit().await?;
    let choices: Vec<_> = found
        .into_iter()
        .take(COMMUNITY_CHOICES as usize)
        .map(|community| {
            json!({
                "id": community.id.to_string(),
                "name": community.name,
                "slug": community.slug,
                "member": community.is_member,
                "drawn": community.has_participated,
            })
        })
        .collect();
    Ok(Json(json!({ "choices": choices })).into_response())
}

pub async fn start_draw_get() -> Redirect {
    Redirect::to("/")
}

pub async fn start_draw(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Form(input): Form<Input>,
) -> Result<Response, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    let community_id = input
        .community_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());
    let community = if let Some(cid) = community_id {
        find_community_by_id(&mut tx, cid).await?
    } else {
        None
    };

    // Query parent post if parent_post_id is provided
    let parent_post = if let Some(ref parent_post_id) = input.parent_post_id {
        let parent_uuid = Uuid::parse_str(parent_post_id).ok();
        if let Some(uuid) = parent_uuid {
            find_post_by_id(&mut tx, uuid).await?
        } else {
            None
        }
    } else {
        None
    };

    // A guest draws on a plain canvas or for a public community, and relays
    // nothing: see `may_draw_in`.
    let user = auth_session.user.as_ref();
    if user.is_none()
        && input
            .parent_post_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
    {
        return Ok(sign_in_instead());
    }
    if let Some(ref community) = community
        && !may_draw_in(&mut tx, user, community).await?
    {
        return Ok(match user {
            None => sign_in_instead(),
            Some(_) => AppError::Forbidden.into_response(),
        });
    }

    // Tegaki, when an account asks for it on a plain canvas. It uploads when
    // it is done and keeps nothing on the device, so it is no painter for a
    // guest, and it knows nothing of a two-tone community's colours. Anyone
    // else who asks for it -- by a stale form, or by hand -- gets Neo
    // Cucumber, as every other tool name does.
    let two_tone = community
        .as_ref()
        .is_some_and(|c| c.background_color.is_some() && c.foreground_color.is_some());
    let template_filename = match input.tool.as_deref() {
        Some("tegaki") if user.is_some() && !two_tone => "draw_post_tegaki.jinja",
        _ => "draw_post_cucumber.jinja",
    };

    let mut painter_config = post_painter_config(
        input.width.parse::<u32>()?,
        input.height.parse::<u32>()?,
        community.as_ref(),
        input.parent_post_id.as_deref(),
        &ftl_lang,
        user,
    );
    // Only the painter that wrote it can carry a drawing on; Tegaki keeps
    // nothing in the browser to carry on with.
    if template_filename == "draw_post_cucumber.jinja"
        && let Some(local_draft_id) = input
            .local_draft_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok())
    {
        painter_config["resume"] = json!({ "localDraftId": local_draft_id.to_string() });
    }
    let painter_config = serde_json::to_string(&painter_config)?;
    let presence = Presence::new(if parent_post.is_some() {
        Activity::Relaying
    } else {
        Activity::Drawing
    })
    .in_community(community.as_ref());
    let rendered = state
        .render_page(
            template_filename,
            common_ctx,
            context! {
                presence => Serde(presence),
                community_name => community.as_ref().map(|c| c.name.clone()),
                tool => input.tool,
                width => input.width.parse::<u32>()?,
                height => input.height.parse::<u32>()?,
                background_color => community.as_ref().and_then(|c| c.background_color.clone()),
                foreground_color => community.as_ref().and_then(|c| c.foreground_color.clone()),
                community_id => input.community_id,
                community_slug => community.as_ref().map(|c| c.slug.clone()),
                parent_post => parent_post,
                parent_post_id => input.parent_post_id,
                painter_config
            },
        )
        .await?;

    Ok(Html(rendered).into_response())
}

/// GET /posts/{id}/continue: the painter, open on a draft to draw on it
/// again. It loads the draft's replay and carries on from its end
/// (`PainterHandle.resume`), and saving replaces the draft's drawing rather
/// than making another post (`continues_post_id` in [`draw_finish`]).
///
/// Only the author's own drafts, and only those this painter recorded; see
/// `ContinuableDraft`. A published drawing is finished, so it is sent to its
/// post, as its publish page sends it.
pub async fn continue_draft(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let mut tx = state.db_pool.begin().await?;
    let Some(draft) = find_continuable_draft(&mut tx, user.id, id).await? else {
        return match find_post_by_id(&mut tx, id).await? {
            Some(post)
                if post.get("author_id").cloned().flatten() == Some(user.id.to_string())
                    && post.get("published_at").cloned().flatten().is_some() =>
            {
                Ok(Redirect::to(&format!("/posts/{}", id)).into_response())
            }
            _ => Err(AppError::NotFound("Draft".to_string())),
        };
    };

    let community = match draft.community_id {
        Some(cid) => find_community_by_id(&mut tx, cid).await?,
        None => None,
    };
    if let Some(ref community) = community
        && !may_draw_in(&mut tx, Some(user), community).await?
    {
        return Err(AppError::Forbidden);
    }
    let parent_post = match draft.parent_post_id {
        Some(parent_id) => find_post_by_id(&mut tx, parent_id).await?,
        None => None,
    };
    let common_ctx = CommonContext::build(&mut tx, Some(user), &ftl_lang).await?;

    let width = u32::try_from(draft.width)?;
    let height = u32::try_from(draft.height)?;
    let parent_post_id = draft.parent_post_id.map(|id| id.to_string());
    let mut painter_config = post_painter_config(
        width,
        height,
        community.as_ref(),
        parent_post_id.as_deref(),
        &ftl_lang,
        Some(user),
    );
    painter_config["resume"] = json!({
        "postId": draft.post_id.to_string(),
        "replayUrl": stored_replay_url(&state.config.r2_public_endpoint_url, &draft.replay_filename),
        "paintDurationMs": draft.paint_duration_ms.max(0),
    });
    let painter_config = serde_json::to_string(&painter_config)?;

    let presence = Presence::new(if parent_post.is_some() {
        Activity::Relaying
    } else {
        Activity::Drawing
    })
    .in_community(community.as_ref());
    let rendered = state
        .render_page(
            "draw_post_cucumber.jinja",
            common_ctx,
            context! {
                presence => Serde(presence),
                community_name => community.as_ref().map(|c| c.name.clone()),
                width,
                height,
                community_id => draft.community_id.map(|id| id.to_string()),
                community_slug => community.as_ref().map(|c| c.slug.clone()),
                parent_post => parent_post,
                parent_post_id,
                painter_config
            },
        )
        .await?;
    Ok(Html(rendered).into_response())
}

/// Where the public bucket serves a replay, as `storage_macro.jinja`'s
/// `stored_url` writes it.
fn stored_replay_url(public_endpoint_url: &str, replay_filename: &str) -> String {
    let prefix: String = replay_filename.chars().take(2).collect();
    format!("{public_endpoint_url}/replay/{prefix}/{replay_filename}")
}

/// Writes `bytes` to the public bucket as `content_type`, which is what the
/// image store answers with (`crate::image_store`).
///
/// The SDK's error is boxed: it is 360 bytes, and every `Result` carrying it
/// would be that size on the success path too.
pub async fn upload_object(
    client: &Client,
    bucket_name: &str,
    bytes: Vec<u8>,
    key: &str,
    checksum_sha256: &str,
    content_type: &str,
) -> Result<PutObjectOutput, Box<SdkError<PutObjectError>>> {
    let body = ByteStream::from(bytes);
    client
        .put_object()
        .bucket(bucket_name)
        .key(key)
        .content_type(content_type)
        .checksum_sha256(checksum_sha256)
        .body(body)
        .send()
        .await
        .map_err(Box::new)
}

/// What the painter reads back after a save: the post to go and publish.
#[derive(Serialize)]
pub struct DrawFinishResponse {
    pub post_id: String,
}

pub async fn draw_finish(
    auth_session: AuthSession,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, AppError> {
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

    let mut width = 0;
    let mut height = 0;
    let mut image_sha256 = String::new();
    let mut replay_sha256 = String::new();
    let mut replay_data = Vec::new();
    let mut community_id = None;
    let mut paint_duration_ms = None;
    let mut security_count = 0;
    let mut tool = String::new();
    let mut parent_post_id = None;
    let mut client_draft_id = None;
    let mut continues_post_id = None;

    while let Some(field) = multipart.next_field().await? {
        let name = field
            .name()
            .ok_or_else(|| AppError::BadRequest("Field has no name".to_string()))?
            .to_string();
        let data = field.bytes().await?;

        if name == "image" {
            let data_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in image data: {}", e)))?;
            let url = DataUrl::process(data_str)
                .map_err(|e| AppError::BadRequest(format!("Invalid data URL: {}", e)))?;
            let (body, _fragment) = url
                .decode_to_vec()
                .map_err(|e| AppError::BadRequest(format!("Failed to decode image: {}", e)))?;
            image_sha256 = digest(&body);

            assert_eq!(url.mime_type().type_, "image");
            assert_eq!(url.mime_type().subtype, "png");

            upload_object(
                &client,
                &state.config.aws_s3_bucket,
                body,
                &format!(
                    "image/{}{}/{}.png",
                    image_sha256
                        .chars()
                        .next()
                        .ok_or_else(|| AppError::BadRequest("Hash is empty".to_string()))?,
                    image_sha256
                        .chars()
                        .nth(1)
                        .ok_or_else(|| AppError::BadRequest("Hash too short".to_string()))?,
                    image_sha256
                ),
                &BASE64.encode(&safe_decode_hash(&image_sha256)?),
                image_store::PNG,
            )
            .await?;
        } else if name == "animation" {
            replay_sha256 = digest(&*data);
            replay_data = data.to_vec();
        } else if name == "community_id" {
            let id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in community_id: {}", e))
            })?;
            if !id_str.is_empty() {
                community_id = Uuid::parse_str(id_str).ok();
            }
        } else if name == "paint_duration_ms" {
            // Measured by the painter when it saved, not from when the page
            // opened: a drawing kept on the device can be sent days later.
            let duration_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in paint_duration_ms: {}", e))
            })?;
            paint_duration_ms =
                Some(duration_str.parse::<u32>().map_err(|e| {
                    AppError::BadRequest(format!("Invalid paint_duration_ms: {}", e))
                })?);
        } else if name == "security_count" {
            let count_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in security_count: {}", e))
            })?;
            security_count = count_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid security_count: {}", e)))?;
        } else if name == "width" {
            let width_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in width: {}", e)))?;
            width = width_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid width: {}", e)))?;
        } else if name == "height" {
            let height_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in height: {}", e)))?;
            height = height_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid height: {}", e)))?;
        } else if name == "tool" {
            tool = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in tool: {}", e)))?
                .to_string();
        } else if name == "parent_post_id" && !data.is_empty() {
            let parent_id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in parent_post_id: {}", e))
            })?;
            parent_post_id = Some(safe_parse_uuid(parent_id_str)?);
        } else if name == "client_draft_id" && !data.is_empty() {
            let draft_id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in client_draft_id: {}", e))
            })?;
            client_draft_id = Some(safe_parse_uuid(draft_id_str)?);
        } else if name == "continues_post_id" && !data.is_empty() {
            let post_id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in continues_post_id: {}", e))
            })?;
            continues_post_id = Some(safe_parse_uuid(post_id_str)?);
        }
    }
    let paint_duration_ms = paint_duration_ms
        .ok_or_else(|| AppError::BadRequest("paint_duration_ms is required".to_string()))?;

    // Tegaki records its own format; every other painter records NEO's.
    let (tool_enum, replay_extension, replay_content_type) = match tool.as_str() {
        "neo" => (Tool::Neo, "pch", image_store::PCH),
        "cucumber" => (Tool::Cucumber, "pch", image_store::PCH),
        "neo-cucumber-offline" => (Tool::NeoCucumber, "pch", image_store::PCH),
        "tegaki" => (Tool::Tegaki, "tgkr", image_store::TGKR),
        _ => return Ok(StatusCode::BAD_REQUEST.into_response()),
    };

    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // This drawing has been here before; answer with what it made then.
    if let Some(draft_id) = client_draft_id
        && let Some(post_id) =
            find_post_id_by_client_draft_id(&mut tx, current_user.id, draft_id).await?
    {
        return existing_post_response(&mut tx, post_id).await;
    }

    // Get first 2 characters for directory prefix
    let replay_prefix = replay_sha256.chars().take(2).collect::<String>();
    if replay_prefix.len() < 2 {
        return Err(AppError::BadRequest("Replay hash too short".to_string()));
    }
    upload_object(
        &client,
        &state.config.aws_s3_bucket,
        replay_data,
        &format!(
            "replay/{}/{}.{}",
            replay_prefix, replay_sha256, replay_extension
        ),
        &BASE64.encode(&safe_decode_hash(&replay_sha256)?),
        replay_content_type,
    )
    .await?;

    let replay_filename = format!("{}.{}", replay_sha256, replay_extension);

    // A draft drawn on again: its drawing is replaced, and the post stays the
    // one it was. One that has been published or thrown away since the page
    // opened is no draft to replace, and the drawing is kept as a new one
    // rather than lost.
    let continued = match continues_post_id {
        Some(post_id) => find_continuable_draft(&mut tx, current_user.id, post_id).await?,
        None => None,
    };
    if let Some(draft) = continued {
        if let Some(cid) = draft.community_id {
            let allowed = match find_community_by_id(&mut tx, cid).await? {
                Some(community) => may_draw_in(&mut tx, Some(current_user), &community).await?,
                None => false,
            };
            if !allowed {
                return Ok(community_not_allowed());
            }
        }
        replace_draft_drawing(
            &mut tx,
            &draft,
            DraftReplacement {
                paint_duration: PgInterval::try_from(
                    Duration::try_milliseconds(i64::from(paint_duration_ms)).unwrap_or_default(),
                )
                .unwrap_or_default(),
                stroke_count: security_count,
                width,
                height,
                image_filename: format!("{}.png", image_sha256),
                replay_filename,
                tool: tool_enum,
                client_draft_id,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok(Json(DrawFinishResponse {
            post_id: draft.post_id.to_string(),
        })
        .into_response());
    }

    // If creating a reply but community_id is not provided, inherit from parent post

    if community_id.is_none()
        && let Some(parent_id) = parent_post_id
        && let Some(parent_post) = find_post_by_id(&mut tx, parent_id).await?
        && let Some(parent_community_id_str) =
            parent_post.get("community_id").and_then(|v| v.as_ref())
    {
        community_id = Uuid::parse_str(parent_community_id_str).ok();
    }

    if let Some(cid) = community_id {
        let allowed = match find_community_by_id(&mut tx, cid).await? {
            Some(community) => may_draw_in(&mut tx, Some(current_user), &community).await?,
            None => false,
        };
        if !allowed {
            return Ok(community_not_allowed());
        }
    }

    let post_draft = PostDraft {
        author_id: current_user.id,
        community_id,
        paint_duration: PgInterval::try_from(
            Duration::try_milliseconds(i64::from(paint_duration_ms)).unwrap_or_default(),
        )
        .unwrap_or_default(),
        stroke_count: security_count,
        width,
        height,
        image_filename: format!("{}.png", image_sha256),
        replay_filename: Some(replay_filename),
        tool: tool_enum,
        parent_post_id,
        client_draft_id,
    };

    let post = match create_post(&mut tx, post_draft).await {
        Ok(post) => post,
        // Another tab sent the same drawing between the lookup above and
        // this insert, and won; its post is the answer to this one too.
        Err(error) if client_draft_id.is_some() && is_unique_violation(&error) => {
            drop(tx);
            let mut tx = db.begin().await?;
            let draft_id = client_draft_id.expect("checked above");
            let post_id = find_post_id_by_client_draft_id(&mut tx, current_user.id, draft_id)
                .await?
                .ok_or_else(|| AppError::Anyhow(error))?;
            return existing_post_response(&mut tx, post_id).await;
        }
        Err(error) => return Err(error.into()),
    };
    let _ = tx.commit().await;

    Ok(Json(DrawFinishResponse {
        post_id: post.id.to_string(),
    })
    .into_response())
}

/// What `draw_finish` says about a post an earlier upload of the same drawing
/// already made, in the same shape as a new one.
async fn existing_post_response(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Response, AppError> {
    find_post_by_id(tx, post_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Post".to_string()))?;
    Ok(Json(DrawFinishResponse {
        post_id: post_id.to_string(),
    })
    .into_response())
}

/// What `draw_finish` answers a drawing for a community this account may not
/// post in.
fn community_not_allowed() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse::new(
            error_codes::COMMUNITY_NOT_ALLOWED,
            "This account cannot post in that community",
        )),
    )
        .into_response()
}

fn is_unique_violation(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<sqlx::Error>()
        .and_then(|error| error.as_database_error())
        .is_some_and(|error| error.is_unique_violation())
}

pub async fn banner_draw_finish(
    auth_session: AuthSession,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<BannerDrawFinishResponse>, AppError> {
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

    let mut width = 0;
    let mut height = 0;
    let mut image_sha256 = String::new();
    let mut replay_sha256 = String::new();
    let mut paint_duration_ms = None;
    let mut security_count = 0;

    while let Some(field) = multipart.next_field().await? {
        let name = field
            .name()
            .ok_or_else(|| AppError::BadRequest("Field has no name".to_string()))?
            .to_string();
        let data = field.bytes().await?;

        if name == "image" {
            let data_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in image data: {}", e)))?;
            let url = DataUrl::process(data_str)
                .map_err(|e| AppError::BadRequest(format!("Invalid data URL: {}", e)))?;
            let (body, _fragment) = url
                .decode_to_vec()
                .map_err(|e| AppError::BadRequest(format!("Failed to decode image: {}", e)))?;
            image_sha256 = digest(&body);

            assert_eq!(url.mime_type().type_, "image");
            assert_eq!(url.mime_type().subtype, "png");

            upload_object(
                &client,
                &state.config.aws_s3_bucket,
                body,
                &format!(
                    "image/{}{}/{}.png",
                    image_sha256
                        .chars()
                        .next()
                        .ok_or_else(|| AppError::BadRequest("Hash is empty".to_string()))?,
                    image_sha256
                        .chars()
                        .nth(1)
                        .ok_or_else(|| AppError::BadRequest("Hash too short".to_string()))?,
                    image_sha256
                ),
                &BASE64.encode(&safe_decode_hash(&image_sha256)?),
                image_store::PNG,
            )
            .await?;
        } else if name == "animation" {
            replay_sha256 = digest(&*data);

            // Get first 2 characters for directory prefix
            let replay_prefix = replay_sha256.chars().take(2).collect::<String>();
            if replay_prefix.len() < 2 {
                return Err(AppError::BadRequest("Replay hash too short".to_string()));
            }
            upload_object(
                &client,
                &state.config.aws_s3_bucket,
                data.to_vec(),
                &format!("replay/{}/{}.pch", replay_prefix, replay_sha256),
                &BASE64.encode(&safe_decode_hash(&replay_sha256)?),
                image_store::PCH,
            )
            .await?;
        } else if name == "paint_duration_ms" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in paint_duration_ms: {}", e))
            })?;
            paint_duration_ms =
                Some(data_str.parse::<u32>().map_err(|e| {
                    AppError::BadRequest(format!("Invalid paint_duration_ms: {}", e))
                })?);
        } else if name == "security_count" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::BadRequest(format!("Invalid UTF-8 in security_count: {}", e))
            })?;
            security_count = data_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid security_count: {}", e)))?;
        } else if name == "width" {
            let data_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in width: {}", e)))?;
            width = data_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid width: {}", e)))?;
        } else if name == "height" {
            let data_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::BadRequest(format!("Invalid UTF-8 in height: {}", e)))?;
            height = data_str
                .parse::<i32>()
                .map_err(|e| AppError::BadRequest(format!("Invalid height: {}", e)))?;
        }
    }
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let paint_duration_ms = paint_duration_ms
        .ok_or_else(|| AppError::BadRequest("paint_duration_ms is required".to_string()))?;

    let banner_draft = BannerDraft {
        author_id: current_user.id,
        paint_duration: PgInterval::try_from(
            Duration::try_milliseconds(i64::from(paint_duration_ms)).unwrap_or_default(),
        )
        .unwrap_or_default(),
        stroke_count: security_count,
        width,
        height,
        image_filename: format!("{}.png", image_sha256),
        replay_filename: Some(format!("{}.pch", replay_sha256)),
    };

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let banner = create_banner(&mut tx, banner_draft).await?;
    let _ = tx.commit().await;

    Ok(Json(BannerDrawFinishResponse {
        banner_id: banner.id.to_string(),
    }))
}

#[derive(Serialize)]
pub struct BannerDrawFinishResponse {
    pub banner_id: String,
}

pub async fn start_banner_draw(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;
    let painter_config = serde_json::to_string(&json!({
        "width": 200,
        "height": 40,
        "locale": ftl_lang.clone(),
        "submission": {
            "kind": "banner",
            "profileUrl": format!("/@{}", current_user.login_name),
        },
        "mode": { "kind": "standard" },
    }))?;
    let rendered = state
        .render(
            "draw_banner.jinja",
            context! {
                presence => Serde(Presence::new(Activity::DrawingBanner)),
                width => 200,
                height => 40,
                current_user => Serde(auth_session.user),
                ftl_lang,
                painter_config,
            },
        )
        .await?;

    Ok(Html(rendered))
}

#[cfg(test)]
mod painter_config_tests {
    use super::post_painter_config;
    use crate::models::community::{Community, CommunityVisibility};
    use crate::models::user::{User, UserRole};
    use uuid::Uuid;

    fn user() -> User {
        User {
            id: Uuid::new_v4(),
            login_name: "relayer".into(),
            password_hash: None,
            display_name: "Relayer".to_string(),
            email: None,
            email_verified_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            banner_id: None,
            preferred_language: None,
            deleted_at: None,
            show_sensitive_content: false,
            role: UserRole::User,
        }
    }

    fn community() -> Community {
        Community {
            id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
            name: "Two Tone".to_string(),
            slug: "two-tone".to_string(),
            description: String::new(),
            visibility: CommunityVisibility::Public,
            updated_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            background_color: Some("#ffffff".to_string()),
            foreground_color: Some("#000000".to_string()),
        }
    }

    /// A signed-in reply, relayed or drawn fresh, has to reach the painter
    /// with its account, or Save keeps it on the device as a guest's drawing
    /// and never uploads it.
    #[test]
    fn a_signed_in_reply_is_uploaded_not_kept_as_a_guests() {
        let user = user();
        let community = community();
        let parent = Uuid::new_v4().to_string();
        let config =
            post_painter_config(640, 480, Some(&community), Some(&parent), "ko", Some(&user));

        assert_eq!(config["userId"], user.id.to_string());
        assert_eq!(config["parentPostId"], parent);
        assert_eq!(config["communityId"], community.id.to_string());
        assert_eq!(config["communityName"], "Two Tone");
        assert_eq!(config["mode"]["kind"], "two-tone");
    }

    #[test]
    fn a_guest_is_told_to_the_painter_as_no_one() {
        let config = post_painter_config(640, 480, None, None, "en", None);
        assert!(config["userId"].is_null());
        assert!(config["communityName"].is_null());
        assert_eq!(config["mode"]["kind"], "standard");
    }

    /// The painter fetches a continued draft's replay from where the pages
    /// link it, so the two have to agree on where that is.
    #[test]
    fn a_continued_replay_is_fetched_from_where_the_pages_link_it() {
        let env = crate::web::handlers::test_support::env();
        let linked = env
            .render_str(
                r#"{% from "storage_macro.jinja" import stored_url %}{{ stored_url("replay", name) }}"#,
                minijinja::context! { r2_public_endpoint_url => "https://r2.example", name => "abcdef.pch" },
            )
            .expect("the macro renders");
        assert_eq!(
            super::stored_replay_url("https://r2.example", "abcdef.pch"),
            linked
        );
    }

    #[test]
    fn only_this_painters_replays_are_continued() {
        use crate::models::post::is_continuable;
        assert!(is_continuable("neo-cucumber", Some("ab.pch")));
        assert!(is_continuable("cucumber", Some("ab.pch")));
        assert!(!is_continuable("neo", Some("ab.pch")));
        assert!(!is_continuable("tegaki", Some("ab.tgkr")));
        assert!(!is_continuable("neo-cucumber", None));
    }
}
