use activitypub_federation::activity_queue::queue_activity;
use activitypub_federation::activity_sending::SendActivityTask;
use activitypub_federation::config::Data;
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::traits::Activity;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{query_as, Postgres, Transaction, Type};
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::community::{Community, CommunityVisibility};
use crate::models::instance::{find_or_create_local_instance, upsert_instance};
use crate::models::nodeinfo;
use crate::models::user::{find_user_by_id, User};
use crate::web::state::AppState;
use crate::AppConfig;

#[derive(Debug)]
struct UserIdRow {
    id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[sqlx(type_name = "actor_type", rename_all = "PascalCase")]
pub enum ActorType {
    Person,
    Service,
    Group,
    Application,
    Organization,
}

impl std::fmt::Display for ActorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActorType::Person => write!(f, "Person"),
            ActorType::Service => write!(f, "Service"),
            ActorType::Group => write!(f, "Group"),
            ActorType::Application => write!(f, "Application"),
            ActorType::Organization => write!(f, "Organization"),
        }
    }
}

/// An actor's IRI as it is stored, and parsed.
///
/// `activitypub_federation` wants `Object::id` as a borrowed `Url`, so the
/// parse happens once, when the row is decoded -- a malformed IRI is a
/// decode error there rather than a panic wherever the id is asked for. The
/// stored text is kept alongside and is what the string side shows, so a
/// query or a `format!` sees exactly the value in the column, not `Url`'s
/// normalisation of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActorIri {
    raw: String,
    url: Url,
}

impl ActorIri {
    pub fn parse(raw: String) -> Result<Self, url::ParseError> {
        let url = Url::parse(&raw)?;
        Ok(Self { raw, url })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn url(&self) -> &Url {
        &self.url
    }
}

impl std::ops::Deref for ActorIri {
    type Target = str;

    fn deref(&self) -> &str {
        &self.raw
    }
}

impl std::fmt::Display for ActorIri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl Type<Postgres> for ActorIri {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, Postgres> for ActorIri {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let raw = <String as sqlx::Decode<Postgres>>::decode(value)?;
        Ok(Self::parse(raw)?)
    }
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub id: Uuid,
    pub iri: ActorIri,
    pub url: String,
    pub r#type: ActorType,
    pub username: String,
    pub instance_host: String,
    pub handle_host: String,
    pub handle: String,
    pub user_id: Option<Uuid>,
    pub community_id: Option<Uuid>,
    pub name: String,
    pub bio_html: String,
    pub automatically_approves_followers: bool,
    pub inbox_url: String,
    pub shared_inbox_url: String,
    pub followers_url: String,
    pub sensitive: bool,
    pub public_key_pem: String,
    pub private_key_pem: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: DateTime<Utc>,
}

impl Actor {
    pub async fn find_by_user_id(
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
    ) -> Result<Option<Actor>> {
        let actor = query_as!(
            Actor,
            r#"
            SELECT 
                id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url, sensitive,
                public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            FROM actors WHERE user_id = $1
            "#,
            user_id
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(actor)
    }

    pub async fn find_by_iri(
        tx: &mut Transaction<'_, Postgres>,
        iri: String,
    ) -> Result<Option<Actor>> {
        let actor = query_as!(
            Actor,
            r#"
            SELECT 
                id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url, sensitive,
                public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            FROM actors WHERE iri = $1
            "#,
            iri
        )
        .fetch_optional(&mut **tx)
        .await?;
        Ok(actor)
    }

    pub async fn find_by_community_id(
        tx: &mut Transaction<'_, Postgres>,
        community_id: Uuid,
    ) -> Result<Option<Actor>> {
        let actor = query_as!(
            Actor,
            r#"
            SELECT 
                id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url, sensitive,
                public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            FROM actors WHERE community_id = $1
            "#,
            community_id
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(actor)
    }

    pub async fn create_or_update_actor(
        tx: &mut Transaction<'_, Postgres>,
        actor: &Actor,
    ) -> Result<Actor> {
        let now = Utc::now();

        // Fetch nodeinfo for the instance to get software name and version
        let nodeinfo = nodeinfo::fetch_nodeinfo(&actor.instance_host)
            .await
            .ok()
            .flatten();
        let (software_name, software_version) = if let Some(nodeinfo) = nodeinfo {
            (
                Some(nodeinfo.software.name),
                Some(nodeinfo.software.version),
            )
        } else {
            (None, None)
        };

        // Upsert the instance first with nodeinfo data
        let _instance = upsert_instance(
            tx,
            &actor.instance_host,
            software_name.as_deref(),
            software_version.as_deref(),
        )
        .await?;

        let created_actor = query_as!(
            Actor,
            r#"
            INSERT INTO actors (
                iri, type, username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url,
                sensitive, public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
            )
            ON CONFLICT (iri) DO UPDATE SET
                username = EXCLUDED.username,
                name = EXCLUDED.name,
                bio_html = EXCLUDED.bio_html,
                automatically_approves_followers = EXCLUDED.automatically_approves_followers,
                inbox_url = EXCLUDED.inbox_url,
                shared_inbox_url = EXCLUDED.shared_inbox_url,
                followers_url = EXCLUDED.followers_url,
                sensitive = EXCLUDED.sensitive,
                public_key_pem = EXCLUDED.public_key_pem,
                url = EXCLUDED.url,
                updated_at = $19
            RETURNING 
                id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url,
                sensitive, public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            "#,
            actor.iri.as_str(),
            actor.r#type as _,
            actor.username,
            actor.instance_host,
            actor.handle_host,
            actor.handle,
            actor.user_id,
            actor.community_id,
            actor.name,
            actor.bio_html,
            actor.automatically_approves_followers,
            actor.inbox_url,
            actor.shared_inbox_url,
            actor.followers_url,
            actor.sensitive,
            actor.public_key_pem,
            actor.private_key_pem,
            actor.url,
            now,
            now,
            now
        )
        .fetch_one(&mut **tx)
        .await?;

        Ok(created_actor)
    }

    pub(crate) async fn send<A>(
        &self,
        activity: A,
        recipients: Vec<Url>,
        use_queue: bool,
        data: &Data<AppState>,
    ) -> Result<(), AppError>
    where
        A: Activity + Serialize + std::fmt::Debug + Send + Sync,
        <A as Activity>::Error: From<anyhow::Error> + From<serde_json::Error>,
    {
        // Print activity
        tracing::info!("Activity: {:?}", activity);

        let context = [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ];

        let activity = WithContext::new(
            activity,
            Value::Array(
                context
                    .into_iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
        );
        // Send through queue in some cases and bypass it in others to test both code paths
        if use_queue {
            queue_activity(&activity, self, recipients, data).await?;
        } else {
            let sends = SendActivityTask::prepare(&activity, self, recipients, data).await?;
            for send in sends {
                send.sign_and_send(data).await?;
            }
        }
        Ok(())
    }
}

pub async fn create_actor_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user: &User,
    config: &AppConfig,
) -> Result<Actor> {
    use activitypub_federation::http_signatures::generate_actor_keypair;

    // Ensure local instance exists
    find_or_create_local_instance(tx, &config.domain, None, None).await?;

    // Generate RSA keypair for ActivityPub
    let keypair = generate_actor_keypair()?;
    let private_key_pem = keypair.private_key;
    let public_key_pem = keypair.public_key;

    let now = Utc::now();

    let iri = format!("https://{}/ap/users/{}", config.domain, user.id);
    let handle = format!("@{}@{}", user.login_name, config.domain);
    let inbox_url = format!("https://{}/ap/users/{}/inbox", config.domain, user.id);
    let shared_inbox_url = format!("https://{}/ap/inbox", config.domain);
    let followers_url = format!("https://{}/ap/users/{}/followers", config.domain, user.id);
    let url = format!("https://{}/@{}", config.domain, user.login_name);

    let actor = query_as!(Actor,
        r#"
        INSERT INTO actors (
            iri, type, username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
        )
        RETURNING 
            id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        "#,
        iri,
        ActorType::Person as _,
        user.login_name,
        config.domain,
        config.domain,
        handle,
        user.id,
        None::<Uuid>, // community_id is None for user actors
        user.display_name,
        "", // bio_html - empty for now
        true, // automatically_approves_followers
        inbox_url,
        shared_inbox_url,
        followers_url,
        false, // sensitive
        public_key_pem,
        private_key_pem,
        url,
        now,
        now,
        now
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(actor)
}

pub async fn backfill_actors_for_existing_users(
    tx: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
) -> Result<usize> {
    // Get user IDs who don't have actors
    let user_ids = query_as!(
        UserIdRow,
        r#"
        SELECT u.id
        FROM users u
        LEFT JOIN actors a ON u.id = a.user_id
        WHERE a.id IS NULL
        "#
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut created_count = 0;
    for row in user_ids {
        if let Some(user_id) = row.id {
            if let Some(user) = find_user_by_id(tx, user_id).await? {
                create_actor_for_user(tx, &user, config).await?;
                created_count += 1;
            }
        }
    }

    Ok(created_count)
}

pub async fn update_actor_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    username: String,
    name: String,
    config: &AppConfig,
) -> Result<Option<Actor>> {
    let handle = format!("@{}@{}", username, config.domain);
    let url = format!("https://{}/@{}", config.domain, username);

    let actor = query_as!(
        Actor,
        r#"
        UPDATE actors 
        SET username = $1, name = $2, handle = $3, url = $4, updated_at = now()
        WHERE user_id = $5
        RETURNING 
            id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        "#,
        username,
        name,
        handle,
        url,
        user_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(actor)
}

pub async fn update_actor_for_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    username: String,
    name: String,
    description: String,
    config: &AppConfig,
) -> Result<Option<Actor>> {
    let handle = format!("@{}@{}", username, config.domain);
    let url = format!("https://{}/communities/@{}", config.domain, username);

    let actor = query_as!(
        Actor,
        r#"
        UPDATE actors 
        SET username = $1, name = $2, handle = $3, url = $4, bio_html = $5, updated_at = now()
        WHERE community_id = $6
        RETURNING 
            id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        "#,
        username,
        name,
        handle,
        url,
        description, // Use community description as bio_html
        community_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(actor)
}

pub async fn create_actor_for_community(
    tx: &mut Transaction<'_, Postgres>,
    community: &Community,
    config: &AppConfig,
) -> Result<Actor> {
    use activitypub_federation::http_signatures::generate_actor_keypair;

    // Ensure local instance exists
    find_or_create_local_instance(tx, &config.domain, None, None).await?;

    // Generate RSA keypair for ActivityPub
    let keypair = generate_actor_keypair()?;
    let private_key_pem = keypair.private_key;
    let public_key_pem = keypair.public_key;

    let now = Utc::now();

    let iri = format!("https://{}/ap/communities/{}", config.domain, community.id);
    let handle = format!("@{}@{}", community.slug, config.domain);
    let inbox_url = format!(
        "https://{}/ap/communities/{}/inbox",
        config.domain, community.id
    );
    let shared_inbox_url = format!("https://{}/ap/inbox", config.domain);
    let followers_url = format!(
        "https://{}/ap/communities/{}/followers",
        config.domain, community.id
    );
    let url = format!("https://{}/communities/{}", config.domain, community.id);

    let actor = query_as!(Actor,
        r#"
        INSERT INTO actors (
            iri, type, username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21
        )
        RETURNING 
            id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
            user_id, community_id, name, bio_html, automatically_approves_followers,
            inbox_url, shared_inbox_url, followers_url,
            sensitive, public_key_pem, private_key_pem, url,
            created_at, updated_at, published_at
        "#,
        iri,
        ActorType::Group as _,
        community.slug,
        config.domain,
        config.domain,
        handle,
        None::<Uuid>, // user_id is None for community actors
        community.id, // community_id for community actors
        community.name,
        community.description, // Use description as bio_html
        true, // automatically_approves_followers
        inbox_url,
        shared_inbox_url,
        followers_url,
        false, // sensitive
        public_key_pem,
        private_key_pem,
        url,
        now,
        now,
        now
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(actor)
}

pub async fn backfill_actors_for_existing_communities(
    tx: &mut Transaction<'_, Postgres>,
    config: &AppConfig,
) -> Result<usize> {
    use crate::models::community::get_communities;

    // Get all communities that don't have actors
    let communities = get_communities(tx).await?;

    let mut created_count = 0;
    for community in communities {
        // Private communities don't federate, so they don't get an actor
        if community.visibility == CommunityVisibility::Private {
            continue;
        }

        // Check if community already has an actor
        let existing_actor = query_as!(
            Actor,
            r#"
            SELECT 
                id, iri as "iri: ActorIri", type as "type: _", username, instance_host, handle_host, handle,
                user_id, community_id, name, bio_html, automatically_approves_followers,
                inbox_url, shared_inbox_url, followers_url, sensitive,
                public_key_pem, private_key_pem, url,
                created_at, updated_at, published_at
            FROM actors 
            WHERE iri = $1
            "#,
            format!("https://{}/ap/communities/{}", config.domain, community.id)
        )
        .fetch_optional(&mut **tx)
        .await?;

        if existing_actor.is_none() {
            create_actor_for_community(tx, &community, config).await?;
            created_count += 1;
            println!("✓ Created actor for community: {}", community.name);
        }
    }

    Ok(created_count)
}
