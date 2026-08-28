use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::banner::{
    activate_banner, delete_banner, find_banner_by_id, list_user_banners,
};
use crate::models::follow::{
    count_followings_by_user_id, find_followings_by_user_id, follow_user, is_following,
    unfollow_user,
};
use crate::models::guestbook_entry::{
    add_guestbook_entry_reply, create_guestbook_entry, delete_guestbook_entry,
    find_guestbook_entries_by_recipient_id, find_guestbook_entry_by_id, GuestbookEntryDraft,
};
use crate::models::link::{
    create_link, delete_link, find_links_by_user_id, update_link_order, LinkDraft,
};
use crate::models::notification::{
    create_notification, get_notification_by_id, get_unread_count, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::{
    find_published_posts_by_author_id, find_published_public_posts_by_author_id,
};
use crate::models::community::{find_community_by_slug, CommunityVisibility};
use crate::web::handlers::community::render_community_page;
use crate::models::user::{find_user_by_id, find_user_by_login_name, AuthSession};
use crate::web::context::CommonContext;
use crate::web::handlers::home::LoadMoreQuery;
use crate::web::responses::{
    PaginationMeta, ProfileBanner, ProfileFollowing, ProfileFollowingsListResponse, ProfileLink,
    ProfilePost, ProfileResponse, ProfileUser,
};
use crate::web::state::AppState;
use anyhow::Error;
use aws_sdk_s3::config::{Credentials as AwsCredentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;
use axum::extract::{Path, Query};
use axum::http::{uri::Uri, HeaderMap};
use axum::response::IntoResponse;
use axum::{extract::State, http::StatusCode, response::Html, response::Json, Form};

use minijinja::context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ExtractFtlLang;

pub async fn do_follow_profile(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    follow_user(&mut tx, current_user.id, user.id).await?;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the user being followed
    let follower_actor = Actor::find_by_user_id(&mut tx, current_user.id).await?;
    if let Some(follower_actor) = follower_actor {
        let recipient_id = user.id;
        match create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id,
                actor_id: follower_actor.id,
                notification_type: NotificationType::Follow,
                post_id: None,
                comment_id: None,
                reaction_iri: None,
                guestbook_entry_id: None,
            },
        )
        .await
        {
            Ok(notification) => {
                tracing::info!("Created follow notification");
                notification_info.push((notification.id, recipient_id));
            }
            Err(e) => tracing::warn!("Failed to create follow notification: {:?}", e),
        }
    }

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

    let template: minijinja::Template<'_, '_> = state.env.get_template("unfollow_button.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        user,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn do_unfollow_profile(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let _ = unfollow_user(&mut tx, current_user.id, user.id).await;

    // Delete follow notification
    let follower_actor = Actor::find_by_user_id(&mut tx, current_user.id).await?;
    if let Some(follower_actor) = follower_actor {
        match sqlx::query!(
            r#"
            DELETE FROM notifications
            WHERE recipient_id = $1
              AND actor_id = $2
              AND notification_type = 'follow'
            "#,
            user.id,
            follower_actor.id
        )
        .execute(&mut *tx)
        .await
        {
            Ok(_) => tracing::info!("Deleted follow notification"),
            Err(e) => tracing::warn!("Failed to delete follow notification: {:?}", e),
        }
    }

    let _ = tx.commit().await;

    let template: minijinja::Template<'_, '_> = state.env.get_template("follow_button.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        user => Some(user),
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn profile(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let published_posts = find_published_posts_by_author_id(&mut tx, user.id).await?;
    use crate::models::community::CommunityVisibility;
    let public_community_posts = published_posts
        .iter()
        .filter(|post| {
            post.community_visibility == Some(CommunityVisibility::Public)
                || post.community_visibility.is_none()
        })
        .collect::<Vec<_>>();
    let private_community_posts = published_posts
        .iter()
        .filter(|post| {
            post.community_visibility != Some(CommunityVisibility::Public)
                && post.community_visibility.is_some()
        })
        .collect::<Vec<_>>();

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let mut is_current_user_following = false;
    if let Some(current_user) = auth_session.user.clone() {
        is_current_user_following = is_following(&mut tx, current_user.id, user.id).await?;
    }

    let followings = find_followings_by_user_id(&mut tx, user.id, 9999, 0, false).await?;

    let banner = match user.banner_id {
        Some(banner_id) => Some(find_banner_by_id(&mut tx, banner_id).await?),
        None => None,
    };

    let links = find_links_by_user_id(&mut tx, user.id).await?;
    let links = links
        .iter()
        .map(|link| {
            let target = if link.url.starts_with(&state.config.base_url) {
                "_self"
            } else {
                "_blank"
            };
            (link, target)
        })
        .collect::<Vec<_>>();

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        links,
        banner,
        is_following => is_current_user_following,
        followings,
        user => Some(user),
        domain => state.config.domain.clone(),
        public_community_posts,
        private_community_posts,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn profile_or_community(
    auth_session: AuthSession,
    headers: HeaderMap,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    uri: Uri,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // First, try to find a user by login_name
    if let Some(user) = find_user_by_login_name(&mut tx, &slug).await? {
        // User found - render profile page
        let published_posts = find_published_posts_by_author_id(&mut tx, user.id).await?;
        let public_community_posts = published_posts
            .iter()
            .filter(|post| {
                post.community_visibility == Some(CommunityVisibility::Public)
                    || post.community_visibility.is_none()
            })
            .collect::<Vec<_>>();
        let private_community_posts = published_posts
            .iter()
            .filter(|post| {
                post.community_visibility != Some(CommunityVisibility::Public)
                    && post.community_visibility.is_some()
            })
            .collect::<Vec<_>>();

        let common_ctx =
            CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

        let mut is_current_user_following = false;
        if let Some(current_user) = auth_session.user.clone() {
            is_current_user_following = is_following(&mut tx, current_user.id, user.id).await?;
        }

        let followings = find_followings_by_user_id(&mut tx, user.id, 9999, 0, false).await?;

        let banner = match user.banner_id {
            Some(banner_id) => Some(find_banner_by_id(&mut tx, banner_id).await?),
            None => None,
        };

        let links = find_links_by_user_id(&mut tx, user.id).await?;
        let links = links
            .iter()
            .map(|link| {
                let target = if link.url.starts_with(&state.config.base_url) {
                    "_self"
                } else {
                    "_blank"
                };
                (link, target)
            })
            .collect::<Vec<_>>();

        let template: minijinja::Template<'_, '_> = state.env.get_template("profile.jinja")?;
        let rendered = template.render(context! {
            current_user => auth_session.user,
            links,
            banner,
            is_following => is_current_user_following,
            followings,
            user => Some(user),
            domain => state.config.domain.clone(),
            public_community_posts,
            private_community_posts,
            draft_post_count => common_ctx.draft_post_count,
            unread_notification_count => common_ctx.unread_notification_count,
            ftl_lang,
        })?;

        return Ok(Html(rendered).into_response());
    }

    // User not found - try to find a community by slug
    if let Some(community) = find_community_by_slug(&mut tx, slug.clone()).await? {
        return render_community_page(
            &mut tx,
            &state,
            &auth_session,
            &headers,
            ftl_lang,
            community,
            uri.path(),
        )
        .await;
    }

    // Neither user nor community found - render 404 page
    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;
    let template: minijinja::Template<'_, '_> = state.env.get_template("404.jinja")?;
    let rendered: String = template.render(context! {
        current_user => auth_session.user,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang,
    })?;
    Ok((StatusCode::NOT_FOUND, Html(rendered)).into_response())
}

pub async fn profile_iframe(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let posts = find_published_public_posts_by_author_id(&mut tx, user.id, 1000, 0).await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_iframe.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        user => Some(user),
        posts,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn profile_banners_iframe(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let followings = find_followings_by_user_id(&mut tx, user.id, 9999, 0, false).await?;

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("profile_banners_iframe.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        followings,
        user => Some(user),
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn do_move_link_down(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, link_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    if user.id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let link = links
        .iter()
        .find(|link| link.id == link_id)
        .ok_or_else(|| AppError::NotFound("Link".to_string()))?;

    let index = link.index;

    update_link_order(&mut tx, link_id, index + 1).await?;
    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let _ = tx.commit().await;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_settings.jinja")?;
    let rendered = template
        .eval_to_state(context! {
            user => auth_session.user,
            links => links,
            ftl_lang,
        })?
        .render_block("links")?;
    Ok(Html(rendered).into_response())
}

pub async fn do_move_link_up(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, link_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    if user.id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let link = links
        .iter()
        .find(|link| link.id == link_id)
        .ok_or_else(|| AppError::NotFound("Link".to_string()))?;

    // link already unwrapped above
    let index = link.index;

    update_link_order(&mut tx, link_id, index - 1).await?;
    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let _ = tx.commit().await;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_settings.jinja")?;
    let rendered = template
        .eval_to_state(context! {
            user => auth_session.user,
            links => links,
            ftl_lang,
        })?
        .render_block("links")?;
    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct AddLinkForm {
    pub url: String,
    pub description: String,
}

pub async fn do_delete_link(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, link_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    if user.id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let _link = links
        .iter()
        .find(|link| link.id == link_id)
        .ok_or_else(|| AppError::NotFound("Link".to_string()))?;

    delete_link(&mut tx, link_id).await?;

    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let _ = tx.commit().await;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_settings.jinja")?;
    let rendered = template
        .eval_to_state(context! {
            user => auth_session.user,
            links => links,
            ftl_lang,
        })?
        .render_block("links")?;
    Ok(Html(rendered).into_response())
}

pub async fn do_add_link(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Form(form): Form<AddLinkForm>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    if user.id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    if user.email_verified_at.is_none() {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let _ = create_link(
        &mut tx,
        LinkDraft {
            user_id: current_user.id,
            url: form.url,
            description: form.description,
        },
    )
    .await;
    let links = find_links_by_user_id(&mut tx, current_user.id).await?;
    let _ = tx.commit().await;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_settings.jinja")?;
    let rendered = template
        .eval_to_state(context! {
            user => auth_session.user,
            links => links,
            ftl_lang,
        })?
        .render_block("links")?;
    Ok(Html(rendered).into_response())
}

pub async fn profile_settings(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_id(&mut tx, current_user.id)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // User is already the current user from auth, no need for ownership check

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let links = find_links_by_user_id(&mut tx, user.id).await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("profile_settings.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        links,
        user => Some(user),
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn banner_management(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_id(&mut tx, current_user.id)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let banners = list_user_banners(&mut tx, user.id).await?;

    tx.commit().await?;

    // Convert banners to include full image URLs
    let banners_with_urls: Vec<_> = banners
        .into_iter()
        .map(|banner| {
            let image_prefix = &banner.image_filename[..2];
            let image_url = format!(
                "{}/image/{}/{}",
                state.config.r2_public_endpoint_url, image_prefix, banner.image_filename
            );
            (banner, image_url)
        })
        .collect();

    let template: minijinja::Template<'_, '_> = state.env.get_template("banner_management.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        user => Some(user),
        banners => banners_with_urls,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct AddGuestbookEntryReplyForm {
    pub content: String,
}

pub async fn do_reply_guestbook_entry(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((login_name, entry_id)): Path<(String, Uuid)>,
    Form(form): Form<AddGuestbookEntryReplyForm>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let entry = find_guestbook_entry_by_id(&mut tx, entry_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Guestbook entry".to_string()))?;

    if entry.recipient_id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let author = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    if author.id != entry.recipient_id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let mut guestbook_entry = find_guestbook_entry_by_id(&mut tx, entry_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Guestbook entry".to_string()))?;

    let replied_at = add_guestbook_entry_reply(&mut tx, entry_id, form.content.clone()).await?;
    guestbook_entry.reply = Some(form.content);
    guestbook_entry.replied_at = Some(replied_at);

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the guestbook entry author (person who originally wrote the entry)
    let replier_actor = Actor::find_by_user_id(&mut tx, current_user.id).await?;
    if let Some(replier_actor) = replier_actor {
        let recipient_id = guestbook_entry.author_id;
        match create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id,
                actor_id: replier_actor.id,
                notification_type: NotificationType::GuestbookReply,
                post_id: None,
                comment_id: None,
                reaction_iri: None,
                guestbook_entry_id: Some(entry_id),
            },
        )
        .await
        {
            Ok(notification) => {
                tracing::info!("Created guestbook reply notification");
                notification_info.push((notification.id, recipient_id));
            }
            Err(e) => tracing::warn!("Failed to create guestbook reply notification: {:?}", e),
        }
    }

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

    let template: minijinja::Template<'_, '_> =
        state.env.get_template("guestbook_entry_reply.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        user => author,
        entry => guestbook_entry,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(Deserialize)]
pub struct CreateGuestbookEntryForm {
    pub content: String,
}

pub async fn do_delete_guestbook_entry(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((login_name, entry_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let entry = find_guestbook_entry_by_id(&mut tx, entry_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Guestbook entry".to_string()))?;

    if entry.author_id != current_user.id && entry.recipient_id != current_user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Check if login_name matches recipient_id
    let recipient = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;
    if recipient.id != entry.recipient_id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let _ = delete_guestbook_entry(&mut tx, entry_id).await;
    let _ = tx.commit().await;

    Ok(StatusCode::OK.into_response())
}

pub async fn do_write_guestbook_entry(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Form(form): Form<CreateGuestbookEntryForm>,
) -> Result<impl IntoResponse, AppError> {
    let current_user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let current_user_id = current_user.id;
    let recipient_user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;
    let recipient_id = recipient_user.id;

    if current_user_id == recipient_id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    let guestbook_entry = create_guestbook_entry(
        &mut tx,
        GuestbookEntryDraft {
            author_id: current_user_id,
            recipient_id,
            content: form.content,
        },
    )
    .await;

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the guestbook owner
    if let Ok(ref entry) = guestbook_entry {
        let author_actor = Actor::find_by_user_id(&mut tx, current_user_id).await?;
        if let Some(author_actor) = author_actor {
            match create_notification(
                &mut tx,
                CreateNotificationParams {
                    recipient_id,
                    actor_id: author_actor.id,
                    notification_type: NotificationType::GuestbookEntry,
                    post_id: None,
                    comment_id: None,
                    reaction_iri: None,
                    guestbook_entry_id: Some(entry.id),
                },
            )
            .await
            {
                Ok(notification) => {
                    tracing::info!("Created guestbook entry notification");
                    notification_info.push((notification.id, recipient_id));
                }
                Err(e) => tracing::warn!("Failed to create guestbook entry notification: {:?}", e),
            }
        }
    }

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

    let template: minijinja::Template<'_, '_> = state.env.get_template("guestbook_entry.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        user => Some(recipient_user),
        entry => guestbook_entry?,
        ftl_lang,
    })?;
    Ok(Html(rendered).into_response())
}

pub async fn guestbook(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    let guestbook_entries = find_guestbook_entries_by_recipient_id(&mut tx, user.id).await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let banner = match user.banner_id {
        Some(banner_id) => Some(find_banner_by_id(&mut tx, banner_id).await?),
        None => None,
    };

    let mut is_current_user_following = false;
    if let Some(current_user) = auth_session.user.clone() {
        is_current_user_following = is_following(&mut tx, current_user.id, user.id).await?;
    }

    let template: minijinja::Template<'_, '_> = state.env.get_template("guestbook.jinja")?;
    let rendered = template.render(context! {
        current_user => auth_session.user,
        banner,
        user => Some(user),
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        is_following => is_current_user_following,
        guestbook_entries,
        ftl_lang,
    })?;

    Ok(Html(rendered).into_response())
}

pub async fn profile_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
    Query(query): Query<LoadMoreQuery>,
) -> Result<Json<ProfileResponse>, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("User not found"))?;

    // Check if current user is following this profile
    let is_following_profile = match &auth_session.user {
        Some(current_user) => is_following(&mut tx, current_user.id, user.id)
            .await
            .unwrap_or(false),
        None => false,
    };

    // Get only public posts
    let public_posts =
        find_published_public_posts_by_author_id(&mut tx, user.id, query.limit, query.offset)
            .await?;

    // Get banner
    let banner = match user.banner_id {
        Some(banner_id) => {
            let banner = find_banner_by_id(&mut tx, banner_id).await?;
            Some(ProfileBanner {
                id: banner.id,
                image_filename: banner.image_filename.clone(),
                image_url: format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url,
                    &banner.image_filename[..2],
                    banner.image_filename
                ),
            })
        }
        None => None,
    };

    // Get followings (only with banners for preview)
    let followings = find_followings_by_user_id(&mut tx, user.id, 9999, 0, true).await?;

    // Get total followings count
    let total_followings = count_followings_by_user_id(&mut tx, user.id).await?;

    // Get links
    let links = find_links_by_user_id(&mut tx, user.id).await?;

    tx.commit().await?;

    // Convert posts to typed structs with minimal fields for thumbnails
    let posts_typed: Vec<ProfilePost> = public_posts
        .into_iter()
        .map(|post| {
            let image_prefix = &post.image_filename[..2];
            ProfilePost {
                id: post.id,
                image_url: format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, post.image_filename
                ),
                image_width: post.image_width,
                image_height: post.image_height,
            }
        })
        .collect();

    // Convert followings to typed structs
    let followings_typed: Vec<ProfileFollowing> = followings
        .into_iter()
        .map(|following| {
            let banner_image_url = following.banner_image_filename.as_ref().map(|filename| {
                let image_prefix = &filename[..2];
                format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, filename
                )
            });

            ProfileFollowing {
                id: following.user_id,
                login_name: following.login_name,
                display_name: following.display_name,
                banner_image_url,
                banner_image_width: following.banner_image_width,
                banner_image_height: following.banner_image_height,
            }
        })
        .collect();

    // Convert links to typed structs
    let links_typed: Vec<ProfileLink> = links
        .into_iter()
        .map(|link| ProfileLink {
            id: link.id,
            url: link.url,
            description: link.description,
        })
        .collect();

    let has_more = posts_typed.len() as i64 == query.limit;

    Ok(Json(ProfileResponse {
        user: ProfileUser {
            id: user.id,
            login_name: user.login_name,
            display_name: user.display_name,
            is_following: is_following_profile,
        },
        banner,
        posts: posts_typed,
        pagination: PaginationMeta {
            offset: query.offset + query.limit,
            limit: query.limit,
            total: None,
            has_more,
        },
        followings: followings_typed,
        total_followings,
        links: links_typed,
    }))
}

/// API endpoint to get paginated followings list for a profile
pub async fn profile_followings_json(
    _auth_session: AuthSession,
    Path(login_name): Path<String>,
    Query(query): Query<LoadMoreQuery>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find user by login name
    let user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // Get followings with pagination
    let followings =
        find_followings_by_user_id(&mut tx, user.id, query.limit, query.offset, false).await?;

    tx.commit().await?;

    // Convert to typed structs
    let followings_typed: Vec<ProfileFollowing> = followings
        .into_iter()
        .map(|f| {
            let banner_image_url = f.banner_image_filename.as_ref().map(|filename| {
                let image_prefix = &filename[..2];
                format!(
                    "{}/image/{}/{}",
                    state.config.r2_public_endpoint_url, image_prefix, filename
                )
            });

            ProfileFollowing {
                id: f.user_id,
                login_name: f.login_name,
                display_name: f.display_name,
                banner_image_url,
                banner_image_width: f.banner_image_width,
                banner_image_height: f.banner_image_height,
            }
        })
        .collect();

    let has_more = followings_typed.len() as i64 == query.limit;

    Ok(Json(ProfileFollowingsListResponse {
        followings: followings_typed,
        pagination: PaginationMeta {
            offset: query.offset + query.limit,
            limit: query.limit,
            total: None,
            has_more,
        },
    })
    .into_response())
}

/// API endpoint to follow a user
pub async fn follow_profile_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let target_user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // Don't allow following yourself
    if user.id == target_user.id {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    follow_user(&mut tx, user.id, target_user.id).await?;

    // Collect notification info to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the user being followed
    let follower_actor = Actor::find_by_user_id(&mut tx, user.id).await?;
    if let Some(follower_actor) = follower_actor {
        let recipient_id = target_user.id;
        match create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id,
                actor_id: follower_actor.id,
                notification_type: NotificationType::Follow,
                post_id: None,
                comment_id: None,
                reaction_iri: None,
                guestbook_entry_id: None,
            },
        )
        .await
        {
            Ok(notification) => {
                tracing::info!("Created follow notification");
                notification_info.push((notification.id, recipient_id));
            }
            Err(e) => tracing::warn!("Failed to create follow notification: {:?}", e),
        }
    }

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

    Ok(StatusCode::OK.into_response())
}

/// API endpoint to unfollow a user
pub async fn unfollow_profile_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(login_name): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let target_user = find_user_by_login_name(&mut tx, &login_name)
        .await?
        .ok_or_else(|| AppError::NotFound("User".to_string()))?;

    unfollow_user(&mut tx, user.id, target_user.id).await?;

    // Delete follow notification
    let follower_actor = Actor::find_by_user_id(&mut tx, user.id).await?;
    if let Some(follower_actor) = follower_actor {
        match sqlx::query!(
            r#"
            DELETE FROM notifications
            WHERE recipient_id = $1
              AND actor_id = $2
              AND notification_type = 'follow'
            "#,
            target_user.id,
            follower_actor.id
        )
        .execute(&mut *tx)
        .await
        {
            Ok(_) => tracing::info!("Deleted follow notification"),
            Err(e) => tracing::warn!("Failed to delete follow notification: {:?}", e),
        }
    }

    tx.commit().await?;

    Ok(StatusCode::OK.into_response())
}

#[derive(Serialize)]
pub struct BannerListResponse {
    pub banners: Vec<BannerListItemResponse>,
}

#[derive(Serialize)]
pub struct BannerListItemResponse {
    pub id: Uuid,
    pub image_url: String,
    pub created_at: String,
    pub is_active: bool,
}

/// API endpoint to list all banners for the current user
pub async fn list_banners_json(
    auth_session: AuthSession,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let banners = list_user_banners(&mut tx, user.id).await?;

    tx.commit().await?;

    // Convert to response format with full image URLs
    let banners_response: Vec<BannerListItemResponse> = banners
        .into_iter()
        .map(|banner| {
            let image_prefix = &banner.image_filename[..2];
            let image_url = format!(
                "{}/image/{}/{}",
                state.config.r2_public_endpoint_url, image_prefix, banner.image_filename
            );

            BannerListItemResponse {
                id: banner.id,
                image_url,
                created_at: banner.created_at.to_rfc3339(),
                is_active: banner.is_active,
            }
        })
        .collect();

    Ok(Json(BannerListResponse {
        banners: banners_response,
    }))
}

/// API endpoint to activate a banner
pub async fn activate_banner_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    activate_banner(&mut tx, user.id, banner_id).await?;

    tx.commit().await?;

    // Return empty JSON object for API clients
    Ok(Json(serde_json::json!({})))
}

/// API endpoint to delete a banner
pub async fn delete_banner_api(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get banner details before deletion
    let banner = find_banner_by_id(&mut tx, banner_id).await?;

    // Verify ownership
    if banner.author_id != user.id {
        return Err(AppError::Unauthorized);
    }

    // Build R2 keys for deletion using banner's image_filename
    let mut keys = vec![format!(
        "image/{}{}/{}",
        banner
            .image_filename
            .chars()
            .next()
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        banner
            .image_filename
            .chars()
            .nth(1)
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        banner.image_filename
    )];

    // Add replay file if it exists
    if let Some(ref replay_filename) = banner.replay_filename {
        keys.push(format!(
            "replay/{}{}/{}",
            replay_filename
                .chars()
                .next()
                .ok_or_else(|| AppError::InvalidFormData("Replay filename too short".to_string()))?,
            replay_filename
                .chars()
                .nth(1)
                .ok_or_else(|| AppError::InvalidFormData("Replay filename too short".to_string()))?,
            replay_filename
        ));
    }

    // Delete objects from R2
    let credentials = AwsCredentials::new(
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
        .map_err(|e| AppError::from(anyhow::anyhow!("Failed to build object identifiers: {}", e)))?;

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

    // Now delete from database (soft delete)
    delete_banner(&mut tx, user.id, banner_id).await?;

    tx.commit().await?;

    // Return empty JSON object for API clients
    Ok(Json(serde_json::json!({})))
}

/// HTML endpoint to activate a banner
/// Render the banner grid for one user.
///
/// Shared by the page and by the two buttons on it, so what a click swaps in
/// is the same markup a reload would have produced — which is what those
/// buttons used to do the expensive way.
async fn render_banner_grid(
    state: &AppState,
    user_id: Uuid,
    ftl_lang: &str,
) -> Result<String, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;
    let banners = list_user_banners(&mut tx, user_id).await?;
    tx.commit().await?;

    let banners_with_urls: Vec<_> = banners
        .into_iter()
        .map(|banner| {
            let image_prefix = &banner.image_filename[..2];
            let image_url = format!(
                "{}/image/{}/{}",
                state.config.r2_public_endpoint_url, image_prefix, banner.image_filename
            );
            (banner, image_url)
        })
        .collect();

    Ok(state
        .env
        .get_template("banner_grid.jinja")?
        .render(context! {
            banners => banners_with_urls,
            ftl_lang,
        })?)
}

pub async fn do_activate_banner(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    activate_banner(&mut tx, user.id, banner_id).await?;

    tx.commit().await?;

    // Two cards changed, not one: this banner gained the active badge and
    // whichever held it lost it. The grid is the smallest honest unit.
    let grid = render_banner_grid(&state, user.id, &ftl_lang).await?;
    Ok(Html(grid).into_response())
}

/// HTML endpoint to delete a banner
pub async fn do_delete_banner(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path(banner_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Require authentication
    let user = auth_session.user.as_ref().ok_or(AppError::Unauthorized)?;

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get banner details before deletion
    let banner = find_banner_by_id(&mut tx, banner_id).await?;

    // Verify ownership
    if banner.author_id != user.id {
        return Err(AppError::Unauthorized);
    }

    // Build R2 keys for deletion using banner's image_filename
    let mut keys = vec![format!(
        "image/{}{}/{}",
        banner
            .image_filename
            .chars()
            .next()
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        banner
            .image_filename
            .chars()
            .nth(1)
            .ok_or_else(|| AppError::InvalidFormData("Image filename too short".to_string()))?,
        banner.image_filename
    )];

    // Add replay file if it exists
    if let Some(ref replay_filename) = banner.replay_filename {
        keys.push(format!(
            "replay/{}{}/{}",
            replay_filename
                .chars()
                .next()
                .ok_or_else(|| AppError::InvalidFormData("Replay filename too short".to_string()))?,
            replay_filename
                .chars()
                .nth(1)
                .ok_or_else(|| AppError::InvalidFormData("Replay filename too short".to_string()))?,
            replay_filename
        ));
    }

    // Delete objects from R2
    let credentials = AwsCredentials::new(
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
        .map_err(|e| AppError::from(anyhow::anyhow!("Failed to build object identifiers: {}", e)))?;

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

    // Now delete from database (soft delete)
    delete_banner(&mut tx, user.id, banner_id).await?;

    tx.commit().await?;

    let grid = render_banner_grid(&state, user.id, &ftl_lang).await?;
    Ok(Html(grid).into_response())
}
