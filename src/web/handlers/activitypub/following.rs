//! Following, and taking it back.

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::traits::{Activity, Actor as ActivityPubFederationActor};

use activitystreams_kinds::activity::{AcceptType, FollowType, UndoType};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::app_error::AppError;
use crate::models::actor::Actor;
use crate::models::follow;
use crate::models::notification::{retract_follow_notifications, BadgeFalls};
use crate::web::state::AppState;

use super::{generate_object_id, EmojiReact, Like};

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    pub(crate) actor: ObjectId<Actor>,
    pub(crate) object: ObjectId<Actor>,
    #[serde(rename = "type")]
    r#type: FollowType,
    id: Url,
}

impl Follow {
    pub fn new(actor: ObjectId<Actor>, object: ObjectId<Actor>, id: Url) -> Follow {
        Follow {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Follow {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    // Ignore clippy false positive: https://github.com/rust-lang/rust-clippy/issues/6446
    #[allow(clippy::await_holding_lock)]
    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("receive: {:?} {:?}", self.actor, self.object);

        // add to followers
        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        // Find the target actor being followed
        tracing::info!("self.object: {:?}", self.object);
        let following_actor = Actor::find_by_iri(&mut tx, self.object.to_string()).await?;
        tracing::info!("following_actor: {:?}", following_actor);

        let following_actor =
            following_actor.ok_or_else(|| anyhow::anyhow!("Target actor not found"))?;

        // Dereference and persist the follower actor
        let follower_actor = self.actor.dereference(data).await?;
        tracing::info!("follower_actor: {:?}", follower_actor);

        let persisted_follower = match Actor::create_or_update_actor(&mut tx, &follower_actor).await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to persist follower actor: {:?}", e);
                return Err(e.into());
            }
        };
        tracing::info!("persisted_follower: {:?}", persisted_follower);

        // Create the follow relationship
        let follow_relation =
            follow::create_follow_by_actor_ids(&mut tx, persisted_follower.id, following_actor.id)
                .await?;
        tracing::info!("follow_relation: {:?}", follow_relation);

        // Commit the transaction before sending accept
        tx.commit().await?;

        // Send back an accept activity
        let id = generate_object_id(data.domain())?;
        let following_actor_object_id = ObjectId::parse(&following_actor.iri)?;
        let accept = Box::new(Accept::new(following_actor_object_id, self, id.clone()));
        following_actor
            .send(accept, vec![follower_actor.shared_inbox_or_inbox()], data)
            .await?;

        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    actor: ObjectId<Actor>,
    object: Follow,
    r#type: AcceptType,
    id: Url,
}

impl Accept {
    pub fn new(actor: ObjectId<Actor>, object: Follow, id: Url) -> Accept {
        Accept {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Accept {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        // Accept activities are typically not processed when received
        // They're sent as responses to Follow activities
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum UndoObject {
    Follow(Box<Follow>),
    Like(Box<Like>),
    EmojiReact(Box<EmojiReact>),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    pub actor: ObjectId<Actor>,
    pub object: UndoObject,
    pub r#type: UndoType,
    pub id: Url,
}

impl Undo {
    pub fn new(actor: ObjectId<Actor>, object: Follow, id: Url) -> Undo {
        Undo {
            actor,
            object: UndoObject::Follow(Box::new(object)),
            r#type: Default::default(),
            id,
        }
    }
}

#[async_trait::async_trait]
impl Activity for Undo {
    type DataType = AppState;
    type Error = AppError;

    fn id(&self) -> &Url {
        &self.id
    }

    fn actor(&self) -> &Url {
        self.actor.inner()
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        tracing::info!("=== RECEIVED UNDO ACTIVITY ===");
        tracing::info!("Actor: {}", self.actor.inner());

        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        match self.object {
            UndoObject::Follow(follow) => {
                tracing::info!("Undo type: Follow");

                // Find the target actor being unfollowed
                let following_actor =
                    Actor::find_by_iri(&mut tx, follow.object.to_string()).await?;
                let following_actor =
                    following_actor.ok_or_else(|| anyhow::anyhow!("Target actor not found"))?;

                // Find the follower actor
                let follower_actor = Actor::find_by_iri(&mut tx, follow.actor.to_string()).await?;
                let follower_actor =
                    follower_actor.ok_or_else(|| anyhow::anyhow!("Follower actor not found"))?;

                // Remove the follow relationship
                follow::unfollow_by_actor_ids(&mut tx, follower_actor.id, following_actor.id)
                    .await?;
                tracing::info!(
                    "Removed follow relationship: {} -> {}",
                    follower_actor.iri,
                    following_actor.iri
                );

                // Delete follow notification
                let mut falls = BadgeFalls::none();
                if let Some(following_user_id) = following_actor.user_id {
                    falls =
                        retract_follow_notifications(&mut tx, following_user_id, follower_actor.id)
                            .await?;
                }

                tx.commit().await?;
                data.push_service.badges_fell(falls);
            }
            UndoObject::Like(like) => {
                tracing::info!("Undo type: Like (removing ❤️ reaction)");
                tracing::info!("Reaction IRI: {}", like.id);

                // Delete reaction by IRI
                use crate::models::reaction::delete_reaction_by_iri;
                if let Some(falls) = delete_reaction_by_iri(&mut tx, like.id.as_str()).await? {
                    tracing::info!("Deleted ❤️ reaction with IRI: {}", like.id);
                    tx.commit().await?;
                    data.push_service.badges_fell(falls);
                } else {
                    tracing::warn!("Failed to delete reaction with IRI: {}", like.id);
                }
            }
            UndoObject::EmojiReact(react) => {
                tracing::info!(
                    "Undo type: EmojiReact (removing {} reaction)",
                    react.content
                );
                tracing::info!("Reaction IRI: {}", react.id);

                // Delete reaction by IRI
                use crate::models::reaction::delete_reaction_by_iri;
                if let Some(falls) = delete_reaction_by_iri(&mut tx, react.id.as_str()).await? {
                    tracing::info!("Deleted {} reaction with IRI: {}", react.content, react.id);
                    tx.commit().await?;
                    data.push_service.badges_fell(falls);
                } else {
                    tracing::warn!("Failed to delete reaction with IRI: {}", react.id);
                }
            }
        }

        tracing::info!("================================");
        Ok(())
    }
}
