//! Reactions to a post.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::community::{find_community_by_id, get_user_role_in_community};
use crate::models::handle::Handle;
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, send_push_for_notification,
    CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::models::reaction::{
    create_reaction, delete_reaction, find_reactions_by_post_id, find_user_reaction,
    get_reaction_counts, normalize_emoji, ReactionDraft,
};
use crate::models::user::AuthSession;
use crate::web::context::CommonContext;
use crate::web::handlers::{parse_id_with_legacy_support, ParsedId};
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use activitypub_federation::traits::Actor as ActivityPubActor;
use axum::extract::Path;
use axum::response::IntoResponse;
use axum::{extract::State, response::Html, Form};
use minijinja::context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::redirect_to_canonical_post;

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
    let emoji = normalize_emoji(&form.emoji);

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
                        return Err(AppError::Forbidden);
                    }
                }
            }
        }
        // Personal posts (community_id is None) are always accessible for reactions
    } else {
        return Err(AppError::NotFound("Post".to_string()));
    }

    // The handle the post's page lives under (`post_page_path`), which the
    // reactions link is beneath: the community's slug if it has one.
    let login_name = post
        .as_ref()
        .and_then(|p| {
            p.get("community_slug")
                .and_then(|slug| slug.as_ref())
                .or_else(|| p.get("login_name").and_then(|l| l.as_ref()))
        })
        .cloned()
        .unwrap_or_default();

    // Something that is not one emoji, or one you have already reacted
    // with, leaves the reactions as they are. htmx swaps an error response
    // in too, so answering with the block unchanged is what keeps it there.
    let emoji = match emoji {
        Some(emoji)
            if find_user_reaction(&mut tx, post_id, actor.id, emoji)
                .await?
                .is_none() =>
        {
            emoji
        }
        _ => {
            let reaction_counts = get_reaction_counts(&mut tx, post_id, Some(actor.id)).await?;
            tx.commit().await?;
            let rendered = state
                .render(
                    "post_reactions.jinja",
                    context! {
                        current_user => auth_session.user,
                        reaction_counts => reaction_counts,
                        post_id => post_id.to_string(),
                        login_name => login_name,
                    },
                )
                .await?;
            return Ok(Html(rendered).into_response());
        }
    };

    let reaction = create_reaction(
        &mut tx,
        ReactionDraft {
            post_id,
            actor_id: actor.id,
            emoji: emoji.to_string(),
        },
        &state.config.domain,
    )
    .await?;

    // Get post author's actor for sending ActivityPub activity
    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    // Collect notification info (id, recipient_id) to send push notifications after commit
    let mut notification_info: Vec<(Uuid, Uuid)> = Vec::new();

    // Create notification for the post author (don't notify if reacting to own post)
    if let Some(post_author_id) = post_author_id
        && post_author_id != user_id
        && let Ok(notification) = create_notification(
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
                    // The number on the bell, for the icon's badge
                    let badge_count = get_badge_count(&mut tx, recipient_id)
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
    if let Some(author_id) = post_author_id
        && author_id != user_id
    {
        // Only send if reacting to someone else's post
        let mut tx = db.begin().await?;
        let post_author_actor = Actor::find_by_user_id(&mut tx, author_id).await?;
        tx.commit().await?;

        if let Some(post_author_actor) = post_author_actor {
            // Build EmojiReact activity
            use crate::web::handlers::activitypub::EmojiReact;

            // The Note by its id, which is what a server that holds it knows it
            // by; the page's address moves with the post's community.
            let post_url = format!("https://{}/ap/posts/{}", state.config.domain, post_id);

            let emoji_react = EmojiReact {
                actor: Some(activitypub_federation::fetch::object_id::ObjectId::parse(
                    &actor.iri,
                )?),
                object: post_url.parse()?,
                content: emoji.to_string(),
                r#type: "EmojiReact".to_string(),
                id: reaction.iri.parse()?,
                to: vec![post_author_actor.iri.to_string()],
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

    let rendered = state
        .render(
            "post_reactions.jinja",
            context! {
                current_user => auth_session.user,
                reaction_counts => reaction_counts,
                post_id => post_id.to_string(),
                login_name => login_name,
            },
        )
        .await?;
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
                        return Err(AppError::Forbidden);
                    }
                }
            }
        }
        // Personal posts (community_id is None) are always accessible for reactions
    } else {
        return Err(AppError::NotFound("Post".to_string()));
    }

    // Find the reaction before deleting (need IRI for Undo activity)
    let existing_reaction = find_user_reaction(&mut tx, post_id, actor.id, &form.emoji).await?;

    let falls = delete_reaction(&mut tx, post_id, actor.id, &form.emoji).await?;
    // The handle the post's page lives under (`post_page_path`), which the
    // reactions link is beneath: the community's slug if it has one.
    let login_name = post
        .as_ref()
        .and_then(|p| {
            p.get("community_slug")
                .and_then(|slug| slug.as_ref())
                .or_else(|| p.get("login_name").and_then(|l| l.as_ref()))
        })
        .cloned()
        .unwrap_or_default();

    // Get post author's actor for sending ActivityPub activity
    let post_author_id = post
        .as_ref()
        .and_then(|p| p.get("author_id"))
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());

    let user_actor_id = Some(actor.id);
    let reaction_counts = get_reaction_counts(&mut tx, post_id, user_actor_id).await?;
    tx.commit().await?;
    state.push_service.badges_fell(falls);

    // Send Undo(EmojiReact) activity to post author
    if let Some(reaction) = existing_reaction
        && let Some(author_id) = post_author_id
        && author_id != user_id
    {
        // Only send if unreacting to someone else's post
        let mut tx = db.begin().await?;
        let post_author_actor = Actor::find_by_user_id(&mut tx, author_id).await?;
        tx.commit().await?;

        if let Some(post_author_actor) = post_author_actor {
            // Build EmojiReact activity (the object being undone)
            use crate::web::handlers::activitypub::{
                generate_object_id, EmojiReact, Undo, UndoObject,
            };

            // The Note by its id, which is what a server that holds it knows it
            // by; the page's address moves with the post's community.
            let post_url = format!("https://{}/ap/posts/{}", state.config.domain, post_id);

            let emoji_react = EmojiReact {
                actor: Some(activitypub_federation::fetch::object_id::ObjectId::parse(
                    &actor.iri,
                )?),
                object: post_url.parse()?,
                content: form.emoji.clone(),
                r#type: "EmojiReact".to_string(),
                id: reaction.iri.parse()?,
                to: vec![post_author_actor.iri.to_string()],
                cc: vec![],
                signature: None,
            };

            // Build Undo activity
            let undo_id = generate_object_id(&state.config.domain)?;
            let undo = Undo {
                actor: activitypub_federation::fetch::object_id::ObjectId::parse(&actor.iri)?,
                object: UndoObject::EmojiReact(Box::new(emoji_react)),
                r#type: activitystreams_kinds::activity::UndoType::Undo,
                id: undo_id,
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

    let rendered = state
        .render(
            "post_reactions.jinja",
            context! {
                current_user => auth_session.user,
                reaction_counts => reaction_counts,
                post_id => post_id.to_string(),
                login_name => login_name,
            },
        )
        .await?;
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
        return Err(AppError::NotFound("Post".to_string()));
    }
    let post = post.ok_or_else(|| AppError::NotFound("Post".to_string()))?;

    let post_data = post.clone();
    let post_login_name = post_data
        .get("login_name")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| AppError::BadRequest("Missing login_name".to_string()))?;

    let community = match post_data
        .get("community_id")
        .and_then(|v| v.as_ref())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(cid) => find_community_by_id(&mut tx, cid).await?,
        None => None,
    };
    if let Some(redirect) = redirect_to_canonical_post(
        &login_name,
        post_login_name,
        community.as_ref(),
        uuid,
        "/reactions",
    ) {
        return Ok(redirect);
    }

    // Get all reactions for this post
    let reactions = find_reactions_by_post_id(&mut tx, uuid).await?;

    let common_ctx = CommonContext::build(&mut tx, auth_session.user.as_ref(), &ftl_lang).await?;

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
        handle: Handle,
        actor_url: String,
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
                .map(|r| ReactionForTemplate {
                    actor_name: r.actor_name,
                    handle: r.handle,
                    actor_url: r.actor_url,
                    created_at: r.created_at.to_rfc3339(),
                })
                .collect();
            EmojiGroup {
                emoji,
                reactions: reactions_for_template,
            }
        })
        .collect();

    let rendered = state.render_page("post_reactions_detail.jinja", common_ctx, context! {
        post_title => post_data.get("title").and_then(|t| t.as_ref()).unwrap_or(&"Untitled".to_string()),
        post_id => post_id,
        login_name => login_name,
        grouped_reactions => grouped_reactions,
    }).await?;

    Ok(Html(rendered).into_response())
}
