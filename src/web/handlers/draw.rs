use crate::app_error::AppError;
use crate::models::banner::{create_banner, BannerDraft};
use crate::models::community::find_community_by_id;
use crate::models::post::{create_post, find_post_by_id, PostDraft, Tool};
use crate::models::user::{update_user_preferred_language, AuthSession};
use crate::web::context::CommonContext;
use crate::web::handlers::{
    detect_preferred_language, safe_decode_hash, safe_parse_uuid, ExtractAcceptLanguage,
    ExtractFtlLang,
};
use crate::web::state::AppState;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::put_object::{PutObjectError, PutObjectOutput};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use axum::response::{IntoResponse, Redirect};
use axum::Json;
use axum::{
    extract::{Multipart, State},
    http::StatusCode,
    response::Html,
    Form,
};
use chrono::Duration;
use data_encoding::BASE64;
use data_url::DataUrl;
use minijinja::context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha256::digest;
use sqlx::postgres::types::PgInterval;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct Input {
    width: String,
    height: String,
    tool: Option<String>,
    community_id: Option<String>,
    parent_post_id: Option<String>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
pub struct InputMobile {
    width: String,
    height: String,
    tool: String,
    community_id: Option<String>,
    parent_post_id: Option<String>,
}

pub async fn start_draw_get() -> Redirect {
    Redirect::to("/")
}

pub async fn start_draw(
    auth_session: AuthSession,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Form(input): Form<Input>,
) -> Result<Html<String>, AppError> {
    let template_filename = "draw_post_cucumber.jinja";

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

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

    let template: minijinja::Template<'_, '_> = state.env.get_template(template_filename)?;
    let painter_mode = match community.as_ref().and_then(|community| {
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
    let painter_config = serde_json::to_string(&json!({
        "width": input.width.parse::<u32>()?,
        "height": input.height.parse::<u32>()?,
        "communityId": input.community_id.clone(),
        "parentPostId": input.parent_post_id.clone(),
        "locale": ftl_lang.clone(),
        "mode": painter_mode,
    }))?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
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
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
        painter_config
    })?;

    Ok(Html(rendered))
}

pub async fn start_draw_mobile(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    Form(input): Form<InputMobile>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Auto-set language preference from browser if not already set
    if let Some(user) = &auth_session.user {
        if user.preferred_language.is_none() {
            if let Some(lang) = detect_preferred_language(&accept_language) {
                if update_user_preferred_language(&mut tx, user.id, Some(lang))
                    .await
                    .is_ok()
                {
                    // Language preference saved, will be committed with the transaction
                }
            }
        }
    }

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

    let template_filename = "draw_post_cucumber_mobile.jinja";

    let template: minijinja::Template<'_, '_> = state.env.get_template(template_filename)?;
    let painter_mode = match community.as_ref().and_then(|community| {
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
    let painter_config = serde_json::to_string(&json!({
        "width": input.width.parse::<u32>()?,
        "height": input.height.parse::<u32>()?,
        "communityId": community_id.map(|id| id.to_string()),
        "parentPostId": input.parent_post_id.clone(),
        "locale": ftl_lang.clone(),
        "mode": painter_mode,
    }))?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        community_name => community.as_ref().map(|c| c.name.clone()),
        tool => &input.tool,
        width => input.width.parse::<u32>()?,
        height => input.height.parse::<u32>()?,
        background_color => community.as_ref().and_then(|c| c.background_color.clone()),
        foreground_color => community.as_ref().and_then(|c| c.foreground_color.clone()),
        community_id => community_id.map(|id| id.to_string()),
        community_slug => community.as_ref().map(|c| c.slug.clone()),
        parent_post => parent_post,
        parent_post_id => input.parent_post_id,
        r2_public_endpoint_url => state.config.r2_public_endpoint_url.clone(),
        ftl_lang,
        painter_config
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn upload_object(
    client: &Client,
    bucket_name: &str,
    bytes: Vec<u8>,
    key: &str,
    checksum_sha256: &str,
) -> Result<PutObjectOutput, SdkError<PutObjectError>> {
    let body = ByteStream::from(bytes);
    client
        .put_object()
        .bucket(bucket_name)
        .key(key)
        .checksum_sha256(checksum_sha256)
        .body(body)
        .send()
        .await
}

#[derive(Serialize)]
pub struct DrawFinishResponse {
    pub community_id: Option<String>,
    pub post_id: String,
    pub image_url: String,
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
    let mut security_timer = 0;
    let mut security_count = 0;
    let mut tool = String::new();
    let mut parent_post_id = None;

    while let Some(field) = multipart.next_field().await? {
        let name = field
            .name()
            .ok_or_else(|| AppError::InvalidFormData("Field has no name".to_string()))?
            .to_string();
        let data = field.bytes().await?;

        if name == "image" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in image data: {}", e))
            })?;
            let url = DataUrl::process(data_str)
                .map_err(|e| AppError::InvalidFormData(format!("Invalid data URL: {}", e)))?;
            let (body, _fragment) = url
                .decode_to_vec()
                .map_err(|e| AppError::InvalidFormData(format!("Failed to decode image: {}", e)))?;
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
                        .ok_or_else(|| AppError::InvalidHash("Hash is empty".to_string()))?,
                    image_sha256
                        .chars()
                        .nth(1)
                        .ok_or_else(|| AppError::InvalidHash("Hash too short".to_string()))?,
                    image_sha256
                ),
                &BASE64.encode(&safe_decode_hash(&image_sha256)?),
            )
            .await?;
        } else if name == "animation" {
            replay_sha256 = digest(&*data);
            replay_data = data.to_vec();
        } else if name == "community_id" {
            let id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in community_id: {}", e))
            })?;
            if !id_str.is_empty() {
                community_id = Uuid::parse_str(id_str).ok();
            }
        } else if name == "security_timer" {
            let timer_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in security_timer: {}", e))
            })?;
            security_timer = timer_str
                .parse::<u128>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid security_timer: {}", e)))?;
        } else if name == "security_count" {
            let count_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in security_count: {}", e))
            })?;
            security_count = count_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid security_count: {}", e)))?;
        } else if name == "width" {
            let width_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::InvalidFormData(format!("Invalid UTF-8 in width: {}", e)))?;
            width = width_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid width: {}", e)))?;
        } else if name == "height" {
            let height_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in height: {}", e))
            })?;
            height = height_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid height: {}", e)))?;
        } else if name == "tool" {
            tool = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::InvalidFormData(format!("Invalid UTF-8 in tool: {}", e)))?
                .to_string();
        } else if name == "parent_post_id" && !data.is_empty() {
            let parent_id_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in parent_post_id: {}", e))
            })?;
            parent_post_id = Some(safe_parse_uuid(parent_id_str)?);
        }
    }
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let duration_ms = since_the_epoch.as_millis() - security_timer;

    if tool == "neo" || tool == "cucumber" || tool == "neo-cucumber-offline" {
        // Get first 2 characters for directory prefix
        let replay_prefix = replay_sha256.chars().take(2).collect::<String>();
        if replay_prefix.len() < 2 {
            return Err(AppError::InvalidHash("Replay hash too short".to_string()));
        }
        upload_object(
            &client,
            &state.config.aws_s3_bucket,
            replay_data,
            &format!("replay/{}/{}.pch", replay_prefix, replay_sha256),
            &BASE64.encode(&safe_decode_hash(&replay_sha256)?),
        )
        .await?;
    } else if tool == "tegaki" {
        // Get first 2 characters for directory prefix
        let replay_prefix = replay_sha256.chars().take(2).collect::<String>();
        if replay_prefix.len() < 2 {
            return Err(AppError::InvalidHash("Replay hash too short".to_string()));
        }
        upload_object(
            &client,
            &state.config.aws_s3_bucket,
            replay_data,
            &format!("replay/{}/{}.tgkr", replay_prefix, replay_sha256),
            &BASE64.encode(&safe_decode_hash(&replay_sha256)?),
        )
        .await?;
    } else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let replay_filename = if tool == "neo" || tool == "cucumber" || tool == "neo-cucumber-offline" {
        format!("{}.pch", replay_sha256)
    } else if tool == "tegaki" {
        format!("{}.tgkr", replay_sha256)
    } else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };

    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    // If creating a reply but community_id is not provided, inherit from parent post
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    if community_id.is_none() {
        if let Some(parent_id) = parent_post_id {
            if let Some(parent_post) = find_post_by_id(&mut tx, parent_id).await? {
                if let Some(parent_community_id_str) = parent_post.get("community_id").and_then(|v| v.as_ref()) {
                    community_id = Uuid::parse_str(parent_community_id_str).ok();
                }
            }
        }
    }

    let tool_enum: Tool = match tool.as_str() {
        "neo" => Tool::Neo,
        "tegaki" => Tool::Tegaki,
        "cucumber" => Tool::Cucumber,
        "neo-cucumber-offline" => Tool::NeoCucumber,
        _ => return Ok(StatusCode::BAD_REQUEST.into_response()),
    };
    let post_draft = PostDraft {
        author_id: current_user.id,
        community_id,
        paint_duration: PgInterval::try_from(
            Duration::try_milliseconds(duration_ms as i64).unwrap_or_default(),
        )
        .unwrap_or_default(),
        stroke_count: security_count,
        width,
        height,
        image_filename: format!("{}.png", image_sha256),
        replay_filename: Some(replay_filename),
        tool: tool_enum,
        parent_post_id,
    };

    let post = create_post(&mut tx, post_draft).await?;
    let _ = tx.commit().await;

    // Construct image URL
    let image_prefix = &image_sha256[0..2];
    let image_url = format!(
        "{}/image/{}/{}.png",
        state.config.r2_public_endpoint_url, image_prefix, image_sha256
    );

    Ok(Json(DrawFinishResponse {
        community_id: community_id.map(|id| id.to_string()),
        post_id: post.id.to_string(),
        image_url,
    })
    .into_response())
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
    let mut security_timer = 0;
    let mut security_count = 0;

    while let Some(field) = multipart.next_field().await? {
        let name = field
            .name()
            .ok_or_else(|| AppError::InvalidFormData("Field has no name".to_string()))?
            .to_string();
        let data = field.bytes().await?;

        if name == "image" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in image data: {}", e))
            })?;
            let url = DataUrl::process(data_str)
                .map_err(|e| AppError::InvalidFormData(format!("Invalid data URL: {}", e)))?;
            let (body, _fragment) = url
                .decode_to_vec()
                .map_err(|e| AppError::InvalidFormData(format!("Failed to decode image: {}", e)))?;
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
                        .ok_or_else(|| AppError::InvalidHash("Hash is empty".to_string()))?,
                    image_sha256
                        .chars()
                        .nth(1)
                        .ok_or_else(|| AppError::InvalidHash("Hash too short".to_string()))?,
                    image_sha256
                ),
                &BASE64.encode(&safe_decode_hash(&image_sha256)?),
            )
            .await?;
        } else if name == "animation" {
            replay_sha256 = digest(&*data);

            // Get first 2 characters for directory prefix
            let replay_prefix = replay_sha256.chars().take(2).collect::<String>();
            if replay_prefix.len() < 2 {
                return Err(AppError::InvalidHash("Replay hash too short".to_string()));
            }
            upload_object(
                &client,
                &state.config.aws_s3_bucket,
                data.to_vec(),
                &format!("replay/{}/{}.pch", replay_prefix, replay_sha256),
                &BASE64.encode(&safe_decode_hash(&replay_sha256)?),
            )
            .await?;
        } else if name == "security_timer" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in security_timer: {}", e))
            })?;
            security_timer = data_str
                .parse::<u128>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid security_timer: {}", e)))?;
        } else if name == "security_count" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in security_count: {}", e))
            })?;
            security_count = data_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid security_count: {}", e)))?;
        } else if name == "width" {
            let data_str = std::str::from_utf8(data.as_ref())
                .map_err(|e| AppError::InvalidFormData(format!("Invalid UTF-8 in width: {}", e)))?;
            width = data_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid width: {}", e)))?;
        } else if name == "height" {
            let data_str = std::str::from_utf8(data.as_ref()).map_err(|e| {
                AppError::InvalidFormData(format!("Invalid UTF-8 in height: {}", e))
            })?;
            height = data_str
                .parse::<i32>()
                .map_err(|e| AppError::InvalidFormData(format!("Invalid height: {}", e)))?;
        }
    }
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    let duration_ms = since_the_epoch.as_millis() - security_timer;

    let banner_draft = BannerDraft {
        author_id: current_user.id,
        paint_duration: PgInterval::try_from(
            Duration::try_milliseconds(duration_ms as i64).unwrap_or_default(),
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

    let image_prefix = &banner.image_filename[..2];
    let image_url = format!(
        "{}/image/{}/{}",
        state.config.r2_public_endpoint_url, image_prefix, banner.image_filename
    );

    Ok(Json(BannerDrawFinishResponse {
        banner_id: banner.id.to_string(),
        image_url,
    }))
}

#[derive(Serialize)]
pub struct BannerDrawFinishResponse {
    pub banner_id: String,
    pub image_url: String,
}

pub async fn start_banner_draw(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let template: minijinja::Template<'_, '_> = state.env.get_template("draw_banner.jinja")?;
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
    let rendered = template.render(context! {
        width => 200,
        height => 40,
        current_user => auth_session.user,
        ftl_lang,
        painter_config,
    })?;

    Ok(Html(rendered))
}

pub async fn start_banner_draw_mobile(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    // Auto-set language preference from browser if not already set
    if let Some(user) = &auth_session.user {
        if user.preferred_language.is_none() {
            if let Some(lang) = detect_preferred_language(&accept_language) {
                let db = &state.db_pool;
                if let Ok(mut tx) = db.begin().await {
                    if update_user_preferred_language(&mut tx, user.id, Some(lang))
                        .await
                        .is_ok()
                    {
                        let _ = tx.commit().await;
                    }
                }
            }
        }
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("draw_banner_mobile.jinja")?;
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
    let rendered = template.render(context! {
        width => 200,
        height => 40,
        current_user => auth_session.user,
        ftl_lang,
        painter_config,
    })?;

    Ok(Html(rendered))
}
