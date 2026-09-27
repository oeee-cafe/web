//! People and communities as ActivityPub actors.

use activitystreams_kinds::actor::{GroupType, PersonType};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::actor::{Actor, ActorIri, ActorType};

use super::is_web_address;

/// An actor's RSA key, as its document publishes it.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PublicKey {
    pub id: Url,
    pub owner: Url,
    pub public_key_pem: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    id: Url,
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
    id: Url,
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

impl ActorObject {
    /// The actor's IRI.
    pub fn id(&self) -> &Url {
        match self {
            Self::Person(person) => &person.id,
            Self::Group(group) => &group.id,
        }
    }
}

/// The local copy of a remote actor, from the document it publishes, which
/// the fetcher has established as served from its own origin.
///
/// # Errors
///
/// When the actor names itself with a handle that is not a plain name.
pub fn remote_actor(json: ActorObject) -> Result<Actor, AppError> {
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
        tracing::info!("Actor {} gave a url that is not a web address: {url}", &id);
        id.clone()
    };

    // Parse instance host from the actor ID URL
    let actor_url = &id;
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

/// The document an actor of ours is served as.
pub fn actor_object(actor: Actor) -> Result<ActorObject, AppError> {
    let public_key = PublicKey {
        id: format!("{}#main-key", actor.iri)
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
        owner: actor
            .iri
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid IRI URL: {}", e))?,
        public_key_pem: actor.public_key_pem,
    };

    let endpoints = serde_json::json!({
        "type": "as:Endpoints",
        "sharedInbox": format!("https://{}/ap/inbox", actor.instance_host)
    });

    match actor.r#type {
        ActorType::Group => Ok(ActorObject::Group(Group {
            id: actor.iri.parse()?,
            r#type: GroupType::Group,
            inbox: actor.inbox_url.parse()?,
            public_key,
            endpoints,
            followers: actor.followers_url.parse()?,
            manually_approves_followers: !actor.automatically_approves_followers,
            name: actor.name,
            outbox: format!("{}/outbox", actor.iri).parse()?,
            preferred_username: actor.username,
            url: actor.url.parse()?,
        })),
        // Handle all other actor types as Person for ActivityPub compatibility
        ActorType::Person
        | ActorType::Service
        | ActorType::Application
        | ActorType::Organization => Ok(ActorObject::Person(Person {
            id: actor.iri.parse()?,
            r#type: PersonType::Person,
            inbox: actor.inbox_url.parse()?,
            public_key,
            endpoints,
            followers: actor.followers_url.parse()?,
            manually_approves_followers: !actor.automatically_approves_followers,
            name: actor.name,
            outbox: format!("{}/outbox", actor.iri).parse()?,
            preferred_username: actor.username,
            url: actor.url.parse()?,
        })),
    }
}
