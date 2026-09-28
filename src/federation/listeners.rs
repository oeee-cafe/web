//! What this site does with the activities other servers send it.
//!
//! Ojak receives them (src/federation/serving.rs registers the inboxes): by
//! the time a listener here runs, the sender is authenticated, the activity
//! is read into ojak's vocabulary, and anything embedded that the sender
//! could not vouch for is a reference. What is left is what only this site
//! can decide: that an activity acts on something its sender owns, and what
//! it does to the database.

use ojak::federation::{Builder, Context, Received};
use ojak_vocab::generated::{
    AnyObject, Create, Delete, EmojiReact, Follow, Like, LinkOrObject, Note, Undo, Update,
};
use ojak_vocab::json::Text;
use serde_json::json;
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::live::LiveEvent;
use crate::models::actor::Actor;
use crate::models::comment::{
    create_comment_from_activitypub, delete_comment_by_iri, find_comment_by_iri,
};
use crate::models::follow;
use crate::models::notification::{
    create_notification, get_badge_count, get_notification_by_id, retract_follow_notifications,
    send_push_for_notification, CreateNotificationParams, NotificationType,
};
use crate::models::post::find_post_by_id;
use crate::models::reaction::{create_reaction_from_activitypub, delete_remote_reaction};
use crate::sanitized_html::SanitizedHtml;
use crate::web::handlers::activitypub::{generate_object_id, remote_actor, ActorObject};
use crate::web::state::AppState;

type Ctx = Context<AppState>;

/// Register this site's listeners.
pub fn register(builder: Builder<AppState>) -> Builder<AppState> {
    // The site's error, for ojak's log and the queue's.
    fn logged(error: AppError) -> anyhow::Error {
        match error {
            AppError::Anyhow(error) => error,
            other => anyhow::anyhow!("{other}"),
        }
    }
    builder
        .on::<Follow, _, _, _>(|ctx, r| async move { on_follow(ctx, r).await.map_err(logged) })
        .on::<Undo, _, _, _>(|ctx, r| async move { on_undo(ctx, r).await.map_err(logged) })
        .on::<Create, _, _, _>(|ctx, r| async move { on_create(ctx, r).await.map_err(logged) })
        .on::<Update, _, _, _>(|ctx, r| async move { on_update(ctx, r).await.map_err(logged) })
        .on::<Delete, _, _, _>(|ctx, r| async move { on_delete(ctx, r).await.map_err(logged) })
        .on::<Like, _, _, _>(|ctx, r| async move { on_like(ctx, r).await.map_err(logged) })
        .on::<EmojiReact, _, _, _>(
            |ctx, r| async move { on_emoji_react(ctx, r).await.map_err(logged) },
        )
}

/// The first thing an activity's `object` names.
fn first_object(objects: &[AnyObject]) -> Option<&AnyObject> {
    objects.first()
}

/// The first value of a text.
fn text(text: &Text) -> Option<&str> {
    text.value
        .as_deref()
        .or_else(|| text.languages.values().next().map(String::as_str))
}

/// The local actor `iri` names, fetched and stored if it is not known yet.
///
/// # Errors
///
/// When it cannot be fetched, is not a person or a group, or the database
/// refuses.
pub async fn resolve_actor(state: &AppState, iri: &Url) -> Result<Actor, AppError> {
    let mut tx = state.db_pool.begin().await?;
    if let Some(actor) = Actor::find_by_iri(&mut tx, iri.to_string()).await? {
        return Ok(actor);
    }
    let document = state
        .fetcher
        .document(iri, None)
        .await
        .map_err(|error| anyhow::Error::new(error).context(format!("fetch actor {iri}")))?;
    let object: ActorObject = serde_json::from_value(document.json)
        .map_err(|error| anyhow::anyhow!("actor {iri}: {error}"))?;
    let actor = Actor::create_or_update_actor(&mut tx, &remote_actor(object)?).await?;
    tx.commit().await?;
    Ok(actor)
}

/// The post of this site's `iri` names, by either of its addresses:
/// `https://domain/@name/{id}` or `https://domain/ap/posts/{id}`.
fn local_post_id(state: &AppState, iri: &str) -> Option<Uuid> {
    let domain = &state.config.domain;
    let id = if let Some(rest) = iri.strip_prefix(&format!("https://{domain}/@")) {
        &rest[rest.find('/')? + 1..]
    } else {
        iri.strip_prefix(&format!("https://{domain}/ap/posts/"))?
    };
    Uuid::parse_str(id).ok()
}

/// Push the notifications `notifications` names, after the transaction that
/// made them.
fn push_later(state: &AppState, notifications: Vec<(Uuid, Uuid)>) {
    if notifications.is_empty() {
        return;
    }
    let push_service = state.push_service.clone();
    let db_pool = state.db_pool.clone();
    tokio::spawn(async move {
        for (notification_id, recipient_id) in notifications {
            let Ok(mut tx) = db_pool.begin().await else {
                continue;
            };
            if let Ok(Some(notification)) =
                get_notification_by_id(&mut tx, notification_id, recipient_id).await
            {
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

/// Someone follows a person or community of this site's, and is accepted.
async fn on_follow(ctx: Ctx, received: Received<Follow>) -> Result<(), AppError> {
    let state = ctx.data();
    let Some(target) = first_object(&received.activity.objects).and_then(AnyObject::id) else {
        return Ok(());
    };
    let mut tx = state.db_pool.begin().await?;
    let Some(following) = Actor::find_by_iri(&mut tx, target.to_string()).await? else {
        return Ok(());
    };
    drop(tx);
    let follower = resolve_actor(state, &received.sender).await?;
    let mut tx = state.db_pool.begin().await?;
    follow::create_follow_by_actor_ids(&mut tx, follower.id, following.id).await?;
    tx.commit().await?;

    // The Follow as it arrived, which is what its sender will recognise.
    let accept = json!({
        "id": generate_object_id(&state.config.domain)?.as_str(),
        "type": "Accept",
        "actor": following.iri.as_str(),
        "object": received.document,
    });
    following
        .send(
            accept,
            vec![follower
                .shared_inbox_or_inbox()
                .map_err(|error| anyhow::anyhow!("follower's inbox: {error}"))?],
            state,
        )
        .await
}

/// Someone takes back a follow or a reaction of their own.
async fn on_undo(ctx: Ctx, received: Received<Undo>) -> Result<(), AppError> {
    let state = ctx.data();
    let sender = received.sender.as_str();
    match first_object(&received.activity.objects) {
        Some(AnyObject::Follow(follow)) => {
            // Only the follower takes a follow back.
            let follower = follow.actors.first().and_then(|actor| actor.id());
            if follower.map(|iri| iri.as_str()) != Some(sender) {
                tracing::warn!(sender, "refused an Undo of someone else's Follow");
                return Ok(());
            }
            let Some(target) = first_object(&follow.objects).and_then(AnyObject::id) else {
                return Ok(());
            };
            let mut tx = state.db_pool.begin().await?;
            let (Some(following), Some(follower)) = (
                Actor::find_by_iri(&mut tx, target.to_string()).await?,
                Actor::find_by_iri(&mut tx, sender.to_owned()).await?,
            ) else {
                return Ok(());
            };
            follow::unfollow_by_actor_ids(&mut tx, follower.id, following.id).await?;
            let falls = match following.user_id {
                Some(user_id) => {
                    retract_follow_notifications(&mut tx, user_id, follower.id).await?
                }
                None => Default::default(),
            };
            tx.commit().await?;
            state.push_service.badges_fell(falls);
        }
        // A reaction, embedded or by its IRI: taken back only if the sender
        // made it.
        Some(object @ (AnyObject::Like(_) | AnyObject::EmojiReact(_) | AnyObject::Iri(_))) => {
            let Some(reaction) = object.id() else {
                return Ok(());
            };
            let mut tx = state.db_pool.begin().await?;
            if let Some(falls) = delete_remote_reaction(&mut tx, reaction.as_str(), sender).await? {
                tx.commit().await?;
                state.push_service.badges_fell(falls);
            }
        }
        _ => {}
    }
    Ok(())
}

/// The markdown and the sanitised HTML of a note: its `source` when that is
/// markdown or plain text, and its content otherwise.
fn note_content(note: &Note) -> (String, Option<SanitizedHtml>) {
    let html = text(&note.content).map(SanitizedHtml::clean);
    let from_source = note.source.as_ref().and_then(|source| {
        matches!(
            source.media_type.as_deref(),
            Some("text/markdown" | "text/plain")
        )
        .then(|| text(&source.content).map(str::to_owned))
        .flatten()
    });
    let markdown = from_source.unwrap_or_else(|| {
        html.clone()
            .map_or_else(|| "No content".to_owned(), SanitizedHtml::into_string)
    });
    (markdown, html)
}

/// A reply to a post of this site's becomes a comment on it.
async fn on_create(ctx: Ctx, received: Received<Create>) -> Result<(), AppError> {
    let state = ctx.data();
    let note = match first_object(&received.activity.objects) {
        Some(AnyObject::Note(note)) => (**note).clone(),
        // A reference: the note is fetched from where its id says it lives.
        Some(AnyObject::Iri(iri)) => {
            let Ok(url) = Url::parse(iri.as_str()) else {
                return Ok(());
            };
            match state.fetcher.lookup_as::<AnyObject>(&url, None).await {
                Ok(typed) => match typed.read.into_value() {
                    AnyObject::Note(note) => *note,
                    _ => return Ok(()),
                },
                Err(error) => {
                    tracing::info!(%url, %error, "could not fetch a created object");
                    return Ok(());
                }
            }
        }
        _ => return Ok(()),
    };
    // A comment is its author's: the note has to say the sender wrote it.
    if !note
        .attributions
        .iter()
        .any(|author| author.id().map(|iri| iri.as_str()) == Some(received.sender.as_str()))
    {
        return Ok(());
    }
    let Some(note_id) = note.id.as_ref() else {
        return Ok(());
    };
    let Some(post_id) = note
        .reply_targets
        .first()
        .and_then(LinkOrObject::id)
        .and_then(|target| local_post_id(state, target.as_str()))
    else {
        return Ok(());
    };

    let mut tx = state.db_pool.begin().await?;
    let Some(post) = find_post_by_id(&mut tx, post_id).await? else {
        return Ok(());
    };
    let author_id = post
        .get("author_id")
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());
    drop(tx);
    let actor = resolve_actor(state, &received.sender).await?;
    let (markdown, html) = note_content(&note);

    let mut tx = state.db_pool.begin().await?;
    let comment = create_comment_from_activitypub(
        &mut tx,
        post_id,
        actor.id,
        markdown,
        html,
        note_id.to_string(),
    )
    .await?;
    let mut notifications = Vec::new();
    if let Some(author_id) = author_id {
        let notification = create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id: author_id,
                actor_id: actor.id,
                notification_type: NotificationType::Comment,
                post_id: Some(post_id),
                comment_id: Some(comment.id),
                reaction_iri: None,
                guestbook_entry_id: None,
            },
        )
        .await?;
        notifications.push((notification.id, author_id));
    }
    tx.commit().await?;
    state
        .live
        .publish(LiveEvent::Comments { post_id, by: None });
    push_later(state, notifications);
    Ok(())
}

/// An actor updates itself: its document is fetched again from its server.
async fn on_update(ctx: Ctx, received: Received<Update>) -> Result<(), AppError> {
    let state = ctx.data();
    let object = first_object(&received.activity.objects).and_then(AnyObject::id);
    if object.map(|iri| iri.as_str()) != Some(received.sender.as_str()) {
        // A note's edits are not followed here, and an actor updates only
        // itself.
        return Ok(());
    }
    let mut tx = state.db_pool.begin().await?;
    if Actor::find_by_iri(&mut tx, received.sender.to_string())
        .await?
        .is_none()
    {
        return Ok(());
    }
    drop(tx);
    let document = state
        .fetcher
        .document(&received.sender, None)
        .await
        .map_err(|error| {
            anyhow::Error::new(error).context(format!("refetch {}", received.sender))
        })?;
    let object: ActorObject = serde_json::from_value(document.json)
        .map_err(|error| anyhow::anyhow!("actor {}: {error}", received.sender))?;
    let mut tx = state.db_pool.begin().await?;
    Actor::create_or_update_actor(&mut tx, &remote_actor(object)?).await?;
    tx.commit().await?;
    Ok(())
}

/// Someone deletes a comment of their own.
async fn on_delete(ctx: Ctx, received: Received<Delete>) -> Result<(), AppError> {
    let state = ctx.data();
    let Some(object) = first_object(&received.activity.objects).and_then(AnyObject::id) else {
        return Ok(());
    };
    // Nothing of this site's is any remote actor's to delete.
    if object
        .as_str()
        .starts_with(&format!("https://{}/", state.config.domain))
    {
        tracing::warn!(sender = %received.sender, %object, "refused a remote Delete of a local object");
        return Ok(());
    }
    let mut tx = state.db_pool.begin().await?;
    let Some(comment) = find_comment_by_iri(&mut tx, object.as_str()).await? else {
        return Ok(());
    };
    let Some(sender) = Actor::find_by_iri(&mut tx, received.sender.to_string()).await? else {
        return Ok(());
    };
    if comment.actor_id != sender.id {
        tracing::warn!(sender = %received.sender, %object, "refused a Delete of someone else's comment");
        return Ok(());
    }
    if let Some(falls) = delete_comment_by_iri(&mut tx, object.as_str()).await? {
        tx.commit().await?;
        state.push_service.badges_fell(falls);
        state.live.publish(LiveEvent::Comments {
            post_id: comment.post_id,
            by: None,
        });
    }
    Ok(())
}

/// A reaction to a post of this site's, from its IRI.
async fn react(
    state: &AppState,
    sender: &Url,
    reaction_iri: Option<&str>,
    object: Option<&AnyObject>,
    emoji: String,
) -> Result<(), AppError> {
    let (Some(reaction_iri), Some(post_id)) = (
        reaction_iri,
        object
            .and_then(AnyObject::id)
            .and_then(|iri| local_post_id(state, iri.as_str())),
    ) else {
        return Ok(());
    };
    let mut tx = state.db_pool.begin().await?;
    let Some(post) = find_post_by_id(&mut tx, post_id).await? else {
        return Ok(());
    };
    let author_id = post
        .get("author_id")
        .and_then(|id| id.as_ref())
        .and_then(|id| Uuid::parse_str(id).ok());
    drop(tx);
    let actor = resolve_actor(state, sender).await?;

    let mut tx = state.db_pool.begin().await?;
    let reaction = create_reaction_from_activitypub(
        &mut tx,
        reaction_iri.to_owned(),
        post_id,
        actor.id,
        emoji,
    )
    .await?;
    let mut notifications = Vec::new();
    if let Some(author_id) = author_id {
        let notification = create_notification(
            &mut tx,
            CreateNotificationParams {
                recipient_id: author_id,
                actor_id: actor.id,
                notification_type: NotificationType::Reaction,
                post_id: Some(post_id),
                comment_id: None,
                reaction_iri: Some(reaction.iri.clone()),
                guestbook_entry_id: None,
            },
        )
        .await?;
        notifications.push((notification.id, author_id));
    }
    tx.commit().await?;
    push_later(state, notifications);
    Ok(())
}

/// A like is a ❤️; one carrying a Unicode emoji, as Misskey sends its
/// reactions, is that emoji. A custom emoji's shortcode is not one this site
/// can show, and stays a ❤️.
async fn on_like(ctx: Ctx, received: Received<Like>) -> Result<(), AppError> {
    let like = &received.activity;
    let emoji = ojak_vocab::meaning::like_reaction(like)
        .and_then(|reaction| crate::models::reaction::normalize_emoji(reaction.content))
        .unwrap_or("❤️")
        .to_owned();
    react(
        ctx.data(),
        &received.sender,
        like.id.as_ref().map(|iri| iri.as_str()),
        first_object(&like.objects),
        emoji,
    )
    .await
}

async fn on_emoji_react(ctx: Ctx, received: Received<EmojiReact>) -> Result<(), AppError> {
    let react_to = &received.activity;
    let Some(reaction) = ojak_vocab::meaning::emoji_reaction(react_to) else {
        return Ok(());
    };
    react(
        ctx.data(),
        &received.sender,
        react_to.id.as_ref().map(|iri| iri.as_str()),
        first_object(&react_to.objects),
        reaction.content.to_owned(),
    )
    .await
}
