//! People and communities as ActivityPub actors.

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::kinds::actor::PersonType;
use activitypub_federation::protocol::public_key::PublicKey;
use activitypub_federation::protocol::verification::verify_domains_match;
use activitypub_federation::traits::{Actor as ActivityPubFederationActor, Object};

use activitystreams_kinds::actor::GroupType;
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::{Actor, ActorIri, ActorType};
use crate::web::state::AppState;

use super::is_web_address;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    id: ObjectId<Actor>,
    r#type: PersonType,
    pub(super) preferred_username: String,
    pub(super) name: String,
    inbox: Url,
    outbox: Url,
    public_key: PublicKey,
    endpoints: serde_json::Value,
    followers: Url,
    manually_approves_followers: bool,
    pub(super) url: Url,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    id: ObjectId<Actor>,
    r#type: GroupType,
    pub(super) preferred_username: String,
    pub(super) name: String,
    inbox: Url,
    outbox: Url,
    public_key: PublicKey,
    endpoints: serde_json::Value,
    followers: Url,
    manually_approves_followers: bool,
    pub(super) url: Url,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum ActorObject {
    Person(Person),
    Group(Group),
}

#[async_trait::async_trait]
impl Object for Actor {
    type DataType = AppState;
    type Kind = ActorObject;
    type Error = AppError;

    fn id(&self) -> &Url {
        self.iri.url()
    }

    async fn read_from_id(
        object_id: Url,
        data: &Data<Self::DataType>,
    ) -> Result<Option<Self>, Self::Error> {
        let db = &data.app_data().db_pool;
        let mut tx = db.begin().await?;

        let actor = Actor::find_by_iri(&mut tx, object_id.to_string()).await?;
        tx.commit().await?;
        Ok(actor)
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        let public_key = PublicKey {
            id: format!("{}#main-key", self.iri)
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
            owner: self
                .iri
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
            public_key_pem: self.public_key_pem,
        };

        let endpoints = serde_json::json!({
            "type": "as:Endpoints",
            "sharedInbox": format!("https://{}/ap/inbox", self.instance_host)
        });

        match self.r#type {
            ActorType::Group => Ok(ActorObject::Group(Group {
                id: ObjectId::parse(&self.iri)?,
                r#type: GroupType::Group,
                inbox: self.inbox_url.parse()?,
                public_key,
                endpoints,
                followers: self.followers_url.parse()?,
                manually_approves_followers: !self.automatically_approves_followers,
                name: self.name,
                outbox: format!("{}/outbox", self.iri).parse()?,
                preferred_username: self.username,
                url: self.url.parse()?,
            })),
            // Handle all other actor types as Person for ActivityPub compatibility
            ActorType::Person
            | ActorType::Service
            | ActorType::Application
            | ActorType::Organization => Ok(ActorObject::Person(Person {
                id: ObjectId::parse(&self.iri)?,
                r#type: PersonType::Person,
                inbox: self.inbox_url.parse()?,
                public_key,
                endpoints,
                followers: self.followers_url.parse()?,
                manually_approves_followers: !self.automatically_approves_followers,
                name: self.name,
                outbox: format!("{}/outbox", self.iri).parse()?,
                preferred_username: self.username,
                url: self.url.parse()?,
            })),
        }
    }

    async fn verify(
        json: &Self::Kind,
        expected_domain: &Url,
        _data: &Data<Self::DataType>,
    ) -> Result<(), Self::Error> {
        let id = match json {
            ActorObject::Person(person) => &person.id,
            ActorObject::Group(group) => &group.id,
        };
        verify_domains_match(id.inner(), expected_domain)?;
        Ok(())
    }

    async fn from_json(
        json: Self::Kind,
        _data: &Data<Self::DataType>,
    ) -> Result<Self, Self::Error> {
        let (
            id,
            inbox,
            public_key,
            endpoints,
            followers,
            manually_approves_followers,
            name,
            preferred_username,
            url,
            actor_type,
        ) = match json {
            ActorObject::Person(person) => (
                person.id,
                person.inbox,
                person.public_key,
                person.endpoints,
                person.followers,
                person.manually_approves_followers,
                person.name,
                person.preferred_username,
                person.url,
                ActorType::Person,
            ),
            ActorObject::Group(group) => (
                group.id,
                group.inbox,
                group.public_key,
                group.endpoints,
                group.followers,
                group.manually_approves_followers,
                group.name,
                group.preferred_username,
                group.url,
                ActorType::Group,
            ),
        };

        // Where a reader is sent by the actor's name: printed as a link's
        // href, so only a web address will do. A `url` is whatever the
        // remote server says, and `javascript:` parses as a Url like any
        // other; the actor's id has been fetched and checked against its
        // domain, so an address that is not http(s) gives way to it
        // (actors_url_is_web holds the table to the same).
        let url = if is_web_address(&url) {
            url
        } else {
            tracing::info!(
                "Actor {} gave a url that is not a web address: {url}",
                id.inner()
            );
            id.inner().clone()
        };

        // Parse instance host from the actor ID URL
        let actor_url = id.inner();
        let instance_host = actor_url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Could not extract host from actor URL"))?
            .to_string();

        // Printed as @name@host beside our own @login_name, so a name that
        // could hide or move the host is refused (is_plain_remote_username).
        if !crate::models::actor::is_plain_remote_username(&preferred_username) {
            return Err(anyhow::anyhow!(
                "Refusing actor {actor_url}: preferredUsername {preferred_username:?} is not a plain name"
            )
            .into());
        }

        // Create handle components
        let handle_host = instance_host.clone();
        let handle = format!("@{}@{}", preferred_username, handle_host);

        // Get shared inbox URL from endpoints if available, otherwise use main inbox
        let shared_inbox_url = endpoints
            .get("sharedInbox")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| inbox.as_str())
            .to_string();

        Ok(Actor {
            name,
            iri: ActorIri::parse(id.to_string())?,
            inbox_url: inbox.to_string(),
            public_key_pem: public_key.public_key_pem,
            private_key_pem: None,
            id: Uuid::new_v4(),
            url: url.to_string(),
            r#type: actor_type,
            username: preferred_username.clone(),
            instance_host,
            handle_host,
            handle,
            user_id: None,
            community_id: None,
            bio_html: String::new(),
            automatically_approves_followers: !manually_approves_followers,
            shared_inbox_url,
            followers_url: followers.to_string(),
            sensitive: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            published_at: chrono::Utc::now(),
        })
    }
}

impl ActivityPubFederationActor for Actor {
    fn public_key_pem(&self) -> &str {
        &self.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        self.private_key_pem.clone()
    }

    fn inbox(&self) -> Url {
        self.inbox_url.parse().expect("Inbox URL should be valid")
    }
}
