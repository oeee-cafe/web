//! Who is in a community: invitations, joining, leaving and removal.

use crate::app_error::AppError;
use crate::models::community::{
    accept_invitation, add_community_member, create_invitation, find_community_by_id,
    find_community_by_slug, get_community_members_with_details, get_invitation_by_id,
    get_pending_invitations_with_invitee_details_for_community, get_user_role_in_community,
    is_user_member, leave_community, reject_invitation, remove_community_member,
    withdraw_invitation, CommunityMemberRole,
};
use crate::models::notification::{
    format_community_invitation_message, get_user_language_preference,
};
use crate::models::user::{find_user_by_login_name, AuthSession};
use crate::web::state::AppState;
use axum::extract::Path;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use axum::{extract::State, http::StatusCode, response::Html, Form};
use axum_messages::Messages;
use minijinja::context;
use serde::Deserialize;
use uuid::Uuid;

use crate::web::context::CommonContext;
use crate::web::i18n::{get_bundle, safe_get_message, ExtractAcceptLanguage, ExtractFtlLang};

// ========== Member Management Endpoints ==========

/// List community members
pub async fn get_members(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Only members can view member list
    let user = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let is_member = is_user_member(&mut tx, user.id, community.id).await?;
    if !is_member {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Fetch members with user details in a single query (no N+1)
    let members = get_community_members_with_details(&mut tx, community.id).await?;

    let members_with_details: Vec<serde_json::Value> = members
        .into_iter()
        .map(|member| {
            serde_json::json!({
                "id": member.id,
                "user_id": member.user_id,
                "login_name": member.login_name,
                "display_name": member.display_name,
                "role": member.role,
                "joined_at": member.joined_at,
            })
        })
        .collect();

    tx.commit().await?;

    Ok(axum::Json(members_with_details).into_response())
}

/// Invite a user to a community
#[derive(Deserialize)]
pub struct InviteUserForm {
    login_name: String,
}

pub async fn invite_user(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    messages: Messages,
    Form(form): Form<InviteUserForm>,
) -> Result<impl IntoResponse, AppError> {
    let user_preferred_language = auth_session
        .user
        .as_ref()
        .and_then(|u| u.preferred_language.clone());
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let inviter = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if user is owner or moderator
    let role = get_user_role_in_community(&mut tx, inviter.id, community.id).await?;
    match role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Find the invitee by login_name
    let invitee = find_user_by_login_name(&mut tx, &form.login_name).await?;
    if invitee.is_none() {
        messages.error(safe_get_message(&bundle, "community-invite-user-not-found"));
        return Ok(
            Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response(),
        );
    }
    let invitee = invitee.ok_or_else(|| AppError::NotFound("User".to_string()))?;

    // Check if user is already a member
    let already_member = is_user_member(&mut tx, invitee.id, community.id).await?;
    if already_member {
        messages.error(safe_get_message(&bundle, "community-invite-already-member"));
        return Ok(
            Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response(),
        );
    }

    // Create invitation
    match create_invitation(&mut tx, community.id, inviter.id, invitee.id).await {
        Ok(_invitation) => {
            // Get invitee's language preference before committing transaction
            let invitee_language = get_user_language_preference(&mut tx, invitee.id)
                .await
                .ok()
                .flatten();

            tx.commit().await?;

            // Send push notification to invitee with localized message
            let (title, body) = format_community_invitation_message(
                "invite",
                invitee_language,
                &inviter.display_name,
                &community.slug,
            );

            let mut data = serde_json::Map::new();
            data.insert(
                "community_id".to_string(),
                serde_json::json!(community.id.to_string()),
            );
            data.insert(
                "community_slug".to_string(),
                serde_json::json!(community.slug),
            );
            data.insert(
                "notification_type".to_string(),
                serde_json::json!("community_invite"),
            );

            tracing::info!(
                "Sending community invitation push notification to user {}: title={}, body={}",
                invitee.id,
                title,
                body
            );

            // The number on the bell, for the icon's badge
            let mut badge_tx = db.begin().await?;
            let unread_count =
                crate::models::notification::get_badge_count(&mut badge_tx, invitee.id)
                    .await
                    .ok();
            let _ = badge_tx.commit().await;

            // Send push notification (don't fail if this errors)
            match state
                .push_service
                .send_notification_to_user(
                    invitee.id,
                    &title,
                    &body,
                    unread_count.map(|c| c as u32), // badge count
                    "/notifications",
                    data,
                )
                .await
            {
                Ok(_) => {
                    tracing::info!(
                        "Successfully sent community invitation push notification to user {}",
                        invitee.id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to send community invitation push notification to user {}: {:?}",
                        invitee.id,
                        e
                    );
                }
            }

            messages.success(safe_get_message(&bundle, "community-invite-success"));

            Ok(Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response())
        }
        Err(e) => {
            // Check if this is a duplicate key constraint error
            if let Some(sqlx::Error::Database(ref err)) = e.downcast_ref::<sqlx::Error>() {
                if err.is_unique_violation() {
                    messages.error(safe_get_message(
                        &bundle,
                        "community-invite-already-invited",
                    ));
                    return Ok(
                        Redirect::to(&format!("/communities/@{}/members", community.slug))
                            .into_response(),
                    );
                }
            }
            // For other errors, propagate them
            Err(e.into())
        }
    }
}

/// Remove a member from a community
pub async fn remove_member(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((slug, user_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let current_user = match auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if current user is owner or moderator
    let current_role = get_user_role_in_community(&mut tx, current_user.id, community.id).await?;
    match current_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Cannot remove the owner
    let target_role = get_user_role_in_community(&mut tx, user_id, community.id).await?;
    if target_role == Some(CommunityMemberRole::Owner) {
        return Ok((StatusCode::BAD_REQUEST, "Cannot remove community owner").into_response());
    }

    // Remove the member
    remove_community_member(&mut tx, community.id, user_id).await?;

    tx.commit().await?;

    // Return empty HTML for HTMX to remove the row
    Ok(Html(String::new()).into_response())
}

// ========== Invitation Endpoints ==========

/// What an invitation's buttons get back from htmx (notifications.jinja): the
/// row goes by its own `hx-swap="delete"`, and this answers with the sentence
/// the redirect would have flashed, as a toast, and the bell corrected, since
/// a pending invitation counts on it.
async fn answered_in_place(
    state: &AppState,
    user_id: Uuid,
    ftl_lang: &str,
    said: &str,
    close: &str,
) -> Result<axum::response::Response, AppError> {
    let toast = crate::web::htmx::toast("success", said, close);
    let badge =
        crate::web::handlers::notifications::nav_notification_badge(state, user_id, ftl_lang)
            .await?;
    Ok(Html(format!("{toast}{badge}")).into_response())
}

/// Accept an invitation
pub async fn do_accept_invitation(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(invitation_id): Path<Uuid>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the invitation
    let invitation = get_invitation_by_id(&mut tx, invitation_id).await?;
    if invitation.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let invitation = invitation.ok_or_else(|| AppError::NotFound("Invitation".to_string()))?;

    // Verify the invitation is for the current user
    if invitation.invitee_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Get community info for validation and push notification
    let community = find_community_by_id(&mut tx, invitation.community_id).await?;
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Store inviter_id before consuming invitation
    let inviter_id = invitation.inviter_id;

    // Accept the invitation
    let falls = accept_invitation(&mut tx, invitation_id).await?;

    // Add user as a member
    add_community_member(
        &mut tx,
        invitation.community_id,
        user.id,
        CommunityMemberRole::Member,
        Some(inviter_id),
    )
    .await?;

    // Get inviter's language preference before committing transaction
    let inviter_language = get_user_language_preference(&mut tx, inviter_id)
        .await
        .ok()
        .flatten();

    tx.commit().await?;
    state.push_service.badges_fell(falls);

    // Send push notification to inviter with localized message
    let (title, body) = format_community_invitation_message(
        "accepted",
        inviter_language,
        &user.display_name,
        &community.slug,
    );

    let mut data = serde_json::Map::new();
    data.insert(
        "community_id".to_string(),
        serde_json::json!(community.id.to_string()),
    );
    data.insert(
        "community_slug".to_string(),
        serde_json::json!(community.slug),
    );
    data.insert(
        "notification_type".to_string(),
        serde_json::json!("invitation_accepted"),
    );

    tracing::info!(
        "Sending invitation accepted push notification to user {}: title={}, body={}",
        inviter_id,
        title,
        body
    );

    // The number on the bell, for the icon's badge
    let mut badge_tx = db.begin().await?;
    let unread_count = crate::models::notification::get_badge_count(&mut badge_tx, inviter_id)
        .await
        .ok();
    let _ = badge_tx.commit().await;

    // Send push notification (don't fail if this errors)
    match state
        .push_service
        .send_notification_to_user(
            inviter_id,
            &title,
            &body,
            unread_count.map(|c| c as u32), // badge count
            &format!("/communities/@{}/members", community.slug),
            data,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(
                "Successfully sent invitation accepted push notification to user {}",
                inviter_id
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to send invitation accepted push notification to user {}: {:?}",
                inviter_id,
                e
            );
        }
    }

    let said = safe_get_message(&bundle, "invitation-accepted");
    if crate::web::htmx::is_htmx(&headers) {
        return answered_in_place(
            &state,
            user.id,
            &ftl_lang,
            &said,
            &safe_get_message(&bundle, "close"),
        )
        .await;
    }
    messages.success(said);

    Ok(Redirect::to("/notifications").into_response())
}

/// Reject an invitation
pub async fn do_reject_invitation(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(invitation_id): Path<Uuid>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get the invitation
    let invitation = get_invitation_by_id(&mut tx, invitation_id).await?;
    if invitation.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let invitation = invitation.ok_or_else(|| AppError::NotFound("Invitation".to_string()))?;

    // Verify the invitation is for the current user
    if invitation.invitee_id != user.id {
        return Ok(StatusCode::FORBIDDEN.into_response());
    }

    // Get community info for push notification
    let community = find_community_by_id(&mut tx, invitation.community_id).await?;
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Store inviter_id before consuming invitation
    let inviter_id = invitation.inviter_id;

    // Reject the invitation
    let falls = reject_invitation(&mut tx, invitation_id).await?;

    // Get inviter's language preference before committing transaction
    let inviter_language = get_user_language_preference(&mut tx, inviter_id)
        .await
        .ok()
        .flatten();

    tx.commit().await?;
    state.push_service.badges_fell(falls);

    // Send push notification to inviter with localized message
    let (title, body) = format_community_invitation_message(
        "declined",
        inviter_language,
        &user.display_name,
        &community.slug,
    );

    let mut data = serde_json::Map::new();
    data.insert(
        "community_id".to_string(),
        serde_json::json!(community.id.to_string()),
    );
    data.insert(
        "community_slug".to_string(),
        serde_json::json!(community.slug),
    );
    data.insert(
        "notification_type".to_string(),
        serde_json::json!("invitation_rejected"),
    );

    tracing::info!(
        "Sending invitation rejected push notification to user {}: title={}, body={}",
        inviter_id,
        title,
        body
    );

    // The number on the bell, for the icon's badge
    let mut badge_tx = db.begin().await?;
    let unread_count = crate::models::notification::get_badge_count(&mut badge_tx, inviter_id)
        .await
        .ok();
    let _ = badge_tx.commit().await;

    // Send push notification (don't fail if this errors)
    match state
        .push_service
        .send_notification_to_user(
            inviter_id,
            &title,
            &body,
            unread_count.map(|c| c as u32), // badge count
            &format!("/communities/@{}/members", community.slug),
            data,
        )
        .await
    {
        Ok(_) => {
            tracing::info!(
                "Successfully sent invitation rejected push notification to user {}",
                inviter_id
            );
        }
        Err(e) => {
            tracing::warn!(
                "Failed to send invitation rejected push notification to user {}: {:?}",
                inviter_id,
                e
            );
        }
    }

    let said = safe_get_message(&bundle, "invitation-rejected");
    if crate::web::htmx::is_htmx(&headers) {
        return answered_in_place(
            &state,
            user.id,
            &ftl_lang,
            &said,
            &safe_get_message(&bundle, "close"),
        )
        .await;
    }
    messages.success(said);

    Ok(Redirect::to("/notifications").into_response())
}

/// Retract/cancel a pending invitation
pub async fn retract_invitation(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path((slug, invitation_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Must be logged in
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    // Check if user is owner or moderator
    let user_role = get_user_role_in_community(&mut tx, user.id, community.id).await?;
    match user_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {}
        _ => return Ok(StatusCode::FORBIDDEN.into_response()),
    }

    // Delete the invitation
    let falls = withdraw_invitation(&mut tx, invitation_id, community.id).await?;

    tx.commit().await?;
    state.push_service.badges_fell(falls);

    // Return empty HTML for HTMX to remove the row
    Ok(Html(String::new()).into_response())
}

/// Render members management page
pub async fn members_page(
    auth_session: AuthSession,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;

    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // For private/unlisted communities, only members can view member list
    // For public communities, anyone can view
    let user_role = if community.visibility != crate::models::community::CommunityVisibility::Public
    {
        // Private or unlisted community - require membership
        let user = match &auth_session.user {
            Some(user) => user,
            None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
        };

        let is_member = is_user_member(&mut tx, user.id, community.id).await?;
        if !is_member {
            return Ok(StatusCode::FORBIDDEN.into_response());
        }

        // Get user's role to determine permissions
        get_user_role_in_community(&mut tx, user.id, community.id).await?
    } else {
        // Public community - anyone can view, but only logged-in members have roles
        match &auth_session.user {
            Some(user) => get_user_role_in_community(&mut tx, user.id, community.id).await?,
            None => None,
        }
    };

    // Fetch members with user details in a single query (no N+1)
    let members = get_community_members_with_details(&mut tx, community.id).await?;

    let members_with_details: Vec<serde_json::Value> = members
        .into_iter()
        .map(|member| {
            serde_json::json!({
                "id": member.id,
                "user_id": member.user_id,
                "login_name": member.login_name,
                "display_name": member.display_name,
                "role": member.role,
                "joined_at": member.joined_at,
            })
        })
        .collect();

    // Fetch pending invitations with invitee details in a single query (no N+1)
    let pending_invitations = match user_role {
        Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator) => {
            let invitations =
                get_pending_invitations_with_invitee_details_for_community(&mut tx, community.id)
                    .await?;
            invitations
                .into_iter()
                .map(|invitation| {
                    serde_json::json!({
                        "id": invitation.id,
                        "invitee_login_name": invitation.invitee_login_name,
                        "invitee_display_name": invitation.invitee_display_name,
                        "created_at": invitation.created_at,
                    })
                })
                .collect()
        }
        _ => Vec::new(),
    };

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

    tx.commit().await?;

    let template = "community_members.jinja";
    let rendered = state.render_page(template, common_ctx, context! {
        community,
        members => members_with_details,
        pending_invitations,
        user_role,
        can_invite => matches!(user_role, Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator)),
        can_remove => matches!(user_role, Some(CommunityMemberRole::Owner) | Some(CommunityMemberRole::Moderator)),
        messages => messages.into_iter().collect::<Vec<_>>(),
    }).await?;

    Ok(Html(rendered).into_response())
}

/// Leave a community (HTMX)
pub async fn do_leave_community(
    auth_session: AuthSession,
    ExtractAcceptLanguage(accept_language): ExtractAcceptLanguage,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    messages: Messages,
) -> Result<impl IntoResponse, AppError> {
    let user = match &auth_session.user {
        Some(u) => u,
        None => return Ok(StatusCode::UNAUTHORIZED.into_response()),
    };

    let user_preferred_language = user.preferred_language.clone();
    let bundle = get_bundle(&accept_language, user_preferred_language);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find community
    let community =
        find_community_by_slug(&mut tx, slug.strip_prefix('@').unwrap_or(&slug).to_string())
            .await?;
    if community.is_none() {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let community = community.ok_or_else(|| AppError::NotFound("Community".to_string()))?;

    // Leave the community (will check if user is owner/member inside)
    match leave_community(&mut tx, community.id, user.id).await {
        Ok(_) => {
            tx.commit().await?;
            messages.success(safe_get_message(&bundle, "community-left-success"));
            Ok(Redirect::to("/communities").into_response())
        }
        Err(e) => {
            let error_msg = e.to_string();
            if error_msg.contains("Owners cannot leave") {
                messages.error(safe_get_message(&bundle, "community-owner-cannot-leave"));
            } else if error_msg.contains("not a member") {
                return Ok(StatusCode::NOT_FOUND.into_response());
            } else {
                messages.error(format!("Error: {}", error_msg));
            }
            Ok(Redirect::to(&format!("/communities/@{}/members", community.slug)).into_response())
        }
    }
}
