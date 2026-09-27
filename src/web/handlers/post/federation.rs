//! Telling followers, and a community's followers, about a post.

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::community::find_community_by_id;
use crate::models::follow;
use crate::web::handlers::activitypub::{
    create_note_from_post, create_updated_note_from_post, generate_object_id, Announce, Create,
    Note, UpdateNote,
};
use crate::web::state::AppState;
use activitypub_federation::fetch::object_id::ObjectId;
use uuid::Uuid;

/// Whether a post in `community` goes out over ActivityPub. A personal post
/// does, to its author's followers; a post in a private community does not.
pub(super) fn community_federates(community: Option<&crate::models::community::Community>) -> bool {
    community.is_none_or(|community| {
        community.visibility != crate::models::community::CommunityVisibility::Private
    })
}

pub(super) async fn send_post_to_followers(
    actor: &Actor,
    post_id: Uuid,
    _title: String,
    _content: String,
    state: &AppState,
) -> Result<Note, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get all followers for this actor
    let followers = follow::find_followers_by_actor_id(&mut tx, actor.id).await?;

    // Use the shared function to create the Note
    let note = create_note_from_post(
        &mut tx,
        post_id,
        actor,
        &state.config.domain,
        &state.config.r2_public_endpoint_url,
    )
    .await?;

    let actor_object_id = ObjectId::parse(&actor.iri)?;
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];
    let published = chrono::Utc::now().to_rfc3339();

    tracing::info!("Note: {:?}", note);

    // Only send to followers if there are any
    if !followers.is_empty() {
        // Create the Create activity
        let activity_id = generate_object_id(&state.config.domain)?;
        let create = Create::new(
            actor_object_id,
            note.clone(),
            activity_id,
            to,
            cc,
            published,
        );

        // Get follower inboxes
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // For now, we'll create a minimal federation config to send activities
            // In a production setup, this would be properly integrated with the federation middleware
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all followers
            actor
                .send(create, follower_inboxes, &federation_data)
                .await?;
            tracing::info!(
                "Sent Create activity for post {} to {} followers",
                post_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No followers found for actor {}, skipping ActivityPub post",
            actor.iri
        );
    }

    tx.commit().await?;
    Ok(note)
}

pub(super) async fn send_post_to_community_followers(
    _user_actor: &Actor,
    community_id: Uuid,
    note: &Note,
    state: &AppState,
) -> Result<(), AppError> {
    // Print note
    tracing::info!("Note: {:?}", note);

    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Find the community's actor, create one if it doesn't exist
    let mut community_actor = Actor::find_by_community_id(&mut tx, community_id).await?;
    if community_actor.is_none() {
        // Community doesn't have an actor yet, create one
        let community = find_community_by_id(&mut tx, community_id).await?;
        if let Some(community) = community {
            tracing::info!(
                "Creating actor for community {} as it doesn't exist",
                community_id
            );
            match crate::models::actor::create_actor_for_community(
                &mut tx,
                &community,
                &state.config,
            )
            .await
            {
                Ok(new_actor) => community_actor = Some(new_actor),
                Err(e) => {
                    tracing::error!(
                        "Failed to create actor for community {}: {:?}",
                        community_id,
                        e
                    );
                    return Ok(());
                }
            }
        } else {
            tracing::error!("Community {} not found", community_id);
            return Ok(());
        }
    }

    if let Some(community_actor) = community_actor {
        // Get all followers for the community actor
        let followers = follow::find_followers_by_actor_id(&mut tx, community_actor.id).await?;

        if followers.is_empty() {
            tracing::info!(
                "No followers found for community actor {}, skipping ActivityPub post",
                community_actor.iri
            );
            tx.commit().await?;
            return Ok(());
        }

        // Create the Announce activity referencing the user's original note
        let note_id = note.id.clone();
        let community_actor_object_id = ObjectId::<Actor>::parse(&community_actor.iri)?;

        let published = chrono::Utc::now().to_rfc3339();

        // For the Announce activity, the audience should be the community's followers
        let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
        let cc = vec![
            format!("{}/followers", community_actor.iri), // Community's followers
        ];

        // Create the Announce activity where the community announces the user's post
        let announce_activity_id = generate_object_id(&state.config.domain)?;
        let announce = Announce::new(
            community_actor_object_id,
            note_id.clone(), // The URL of the original post being announced
            announce_activity_id,
            to.clone(),
            cc.clone(),
            published.clone(),
        );

        // Get follower inboxes (community followers)
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // Create federation config to send activities
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all community followers using the community actor (announcing the user's post)
            community_actor
                .send(announce, follower_inboxes, &federation_data)
                .await?;
            tracing::info!(
                "Sent Announce activity for note {} to {} community followers",
                note_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No actor found for community {}, skipping community followers ActivityPub",
            community_id
        );
    }

    tx.commit().await?;
    Ok(())
}

pub(super) async fn send_post_update_to_followers(
    actor: &Actor,
    post_id: Uuid,
    state: &AppState,
) -> Result<(), AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    // Get all followers for this actor
    let followers = follow::find_followers_by_actor_id(&mut tx, actor.id).await?;

    // Use the function to create the updated Note with timestamp
    let note = create_updated_note_from_post(
        &mut tx,
        post_id,
        actor,
        &state.config.domain,
        &state.config.r2_public_endpoint_url,
    )
    .await?;

    let actor_object_id = ObjectId::parse(&actor.iri)?;
    let to = vec!["https://www.w3.org/ns/activitystreams#Public".to_string()];
    let cc = vec![format!("{}/followers", actor.iri)];
    let published = chrono::Utc::now().to_rfc3339();

    tracing::info!("Updated Note: {:?}", note);

    // Only send to followers if there are any
    if !followers.is_empty() {
        // Create the Update activity for the Note
        let activity_id = generate_object_id(&state.config.domain)?;
        let update = UpdateNote::new(actor_object_id, note, activity_id, to, cc, published);

        // Get follower inboxes
        let follower_inboxes: Vec<url::Url> = followers
            .iter()
            .map(|follower| follower.inbox_url.parse())
            .collect::<Result<Vec<_>, _>>()?;

        if !follower_inboxes.is_empty() {
            // Create federation config to send activities
            let federation_config = activitypub_federation::config::FederationConfig::builder()
                .domain(&state.config.domain)
                .app_data(state.clone())
                .build()
                .await?;
            let federation_data = federation_config.to_request_data();

            // Send to all followers
            actor
                .send(update, follower_inboxes, &federation_data)
                .await?;
            tracing::info!(
                "Sent Update activity for post {} to {} followers",
                post_id,
                followers.len()
            );
        }
    } else {
        tracing::info!(
            "No followers to send Update activity to for post {}",
            post_id
        );
    }

    tx.commit().await?;
    Ok(())
}
