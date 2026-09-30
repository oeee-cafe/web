//! What other servers fetch — actors, posts, their collections, WebFinger
//! and NodeInfo — served by ojak.
//!
//! Each route is a dispatcher registered with the template that also builds
//! its URIs, so an actor cannot name a collection that is not served. A
//! request to one of these paths that asks for a page rather than
//! ActivityPub goes on to the site's own routes, which send a browser to the
//! page. Incoming activities are received by the listeners in
//! `listeners.rs`.

use crate::models::actor::{create_actor_for_user, Actor};
use crate::models::community::{find_community_by_id, find_community_by_slug, CommunityVisibility};
use crate::models::user::{find_user_by_id, find_user_by_login_name};
use crate::web::handlers::activitypub::{actor_object, create_note_from_post};
use crate::web::state::AppState;
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use ojak::federation::{
    ActorRef, Collection, Context, Federation, First, Found, NodeInfo, Page, Software, Values,
};
use serde_json::Value;
use url::Url;
use uuid::Uuid;

type Ctx = Context<AppState>;

/// The federation this site serves.
///
/// # Errors
///
/// When the configured domain is not a host, or a template is wrong.
pub fn federation(
    domain: &str,
    fetcher: std::sync::Arc<ojak::fetch::Fetcher>,
    kv: ojak_postgres::PostgresKvStore,
) -> anyhow::Result<Federation<AppState>> {
    let origin = Url::parse(&format!("https://{domain}")).context("the configured domain")?;
    let builder = Federation::builder()
        .origin(origin)
        .inbox("person", "/ap/users/{user_id}/inbox")
        .inbox("group", "/ap/communities/{community_id}/inbox")
        .shared_inbox("/ap/inbox")
        // Keys, and the ids of activities already received, are kept a day.
        // This site fetches unsigned: it has no instance actor to sign as.
        .signed_fetch(
            fetcher,
            kv,
            std::time::Duration::from_secs(24 * 60 * 60),
            |_| async { Ok::<_, std::convert::Infallible>(None) },
        )
        .inbox_queue(|state: &AppState| Some(state.inbox_queue.clone()))
        .actor("person", "/ap/users/{user_id}", person)
        .actor("group", "/ap/communities/{community_id}", group)
        .object("note", "/ap/posts/{post_id}", note)
        // A post's page, which is also where people and other servers find
        // it, as Mastodon's are: `name` is its author's or its community's,
        // and `note` answers by the id alone, as the page redirects any other
        // name to the right one (models/post/urls.rs).
        .object_alias("note", "/@{name}/{post_id}")
        .collection(
            "followers",
            "/ap/users/{user_id}/followers",
            followers(Owner::User),
        )
        .collection(
            "community_followers",
            "/ap/communities/{community_id}/followers",
            followers(Owner::Community),
        )
        .collection("outbox", "/ap/users/{user_id}/outbox", outbox(Owner::User))
        .collection(
            "community_outbox",
            "/ap/communities/{community_id}/outbox",
            outbox(Owner::Community),
        )
        .handle(|ctx: Ctx, name: String| async move { by_name(&ctx, &name).await })
        .map_alias(|ctx: Ctx, url: Url| async move {
            // A profile page, /@name, names whom a handle does.
            match url.path().strip_prefix("/@") {
                Some(name) if !name.is_empty() && !name.contains('/') => by_name(&ctx, name).await,
                _ => Ok(None),
            }
        })
        .nodeinfo(nodeinfo)
        // A reply to a post here, addressed to its author's followers, is
        // passed on to them (ActivityPub §7.1.2), signed by the author.
        .forward(|ctx: Ctx, forward: ojak::federation::Forward| async move {
            forward_to_followers(&ctx, forward).await
        })
        // Another server being down, gone or wrong is not a bug here, and
        // arrives at whatever rate the fediverse sends it.
        .on_error(|error| {
            let remote = std::iter::successors(
                Some(&**error as &(dyn std::error::Error + 'static)),
                |cause| cause.source(),
            )
            .any(crate::app_error::is_remote);
            if remote {
                tracing::warn!(error = %error, "ActivityPub, from another server");
            } else {
                tracing::error!(error = %error, "ActivityPub");
            }
        });
    super::listeners::register(builder)
        .build()
        .map_err(|error| anyhow::anyhow!("{error}"))
}

/// Forward `forward`'s activity to the followers of the people and
/// communities here whose followers collections it names, as it arrived.
/// This site is no portable actor's gateway, so there is nothing to forward
/// to gateways.
async fn forward_to_followers(ctx: &Ctx, forward: ojak::federation::Forward) -> anyhow::Result<()> {
    let ojak::federation::ForwardTo::Collections(collections) = forward.to else {
        return Ok(());
    };
    let state = ctx.data();
    for collection in collections {
        let Some(id) = uuid(&collection.identifier) else {
            continue;
        };
        let mut tx = state.db_pool.begin().await?;
        let actor = match collection.kind.as_str() {
            "followers" => Actor::find_by_user_id(&mut tx, id).await?,
            "community_followers" => Actor::find_by_community_id(&mut tx, id).await?,
            _ => None,
        };
        let Some(actor) = actor else {
            continue;
        };
        let inboxes =
            crate::models::follow::get_follower_shared_inboxes_for_actor(&mut tx, actor.id).await?;
        tx.commit().await?;
        let inboxes: Vec<Url> = inboxes
            .iter()
            .filter_map(|inbox| Url::parse(inbox).ok())
            .collect();
        crate::federation::send(
            &state.deliverer,
            &state.config.domain,
            &actor,
            &forward.activity,
            inboxes,
        )
        .await
        .map_err(|error| anyhow::anyhow!("queueing a forward: {error}"))?;
    }
    Ok(())
}

/// The site's error, for ojak's log.
fn app(error: crate::app_error::AppError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

/// Every path here names things by UUID; anything else names nothing.
fn uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

/// What `table` says of the row `id`: absent, deleted and when, or there.
async fn row_state(
    ctx: &Ctx,
    table: &'static str,
    id: Uuid,
) -> anyhow::Result<Option<Option<DateTime<Utc>>>> {
    // `table` is one of the literals below, never input.
    Ok(
        sqlx::query_scalar(&format!("SELECT deleted_at FROM {table} WHERE id = $1"))
            .bind(id)
            .fetch_optional(&ctx.data().db_pool)
            .await?,
    )
}

async fn person(ctx: Ctx, user_id: String) -> anyhow::Result<Found<Value>> {
    let Some(user_id) = uuid(&user_id) else {
        return Ok(Found::NotFound);
    };
    match row_state(&ctx, "users", user_id).await? {
        None => return Ok(Found::NotFound),
        Some(Some(deleted_at)) => return Ok(Found::Gone(Some(deleted_at))),
        Some(None) => {}
    }
    let mut tx = ctx.data().db_pool.begin().await?;
    match Actor::find_by_user_id(&mut tx, user_id).await? {
        Some(actor) => Ok(Found::Found(serde_json::to_value(
            actor_object(actor).map_err(app)?,
        )?)),
        None => Ok(Found::NotFound),
    }
}

async fn group(ctx: Ctx, community_id: String) -> anyhow::Result<Found<Value>> {
    let Some(community_id) = uuid(&community_id) else {
        return Ok(Found::NotFound);
    };
    match row_state(&ctx, "communities", community_id).await? {
        None => return Ok(Found::NotFound),
        Some(Some(deleted_at)) => return Ok(Found::Gone(Some(deleted_at))),
        Some(None) => {}
    }
    let mut tx = ctx.data().db_pool.begin().await?;
    match Actor::find_by_community_id(&mut tx, community_id).await? {
        Some(actor) => Ok(Found::Found(serde_json::to_value(
            actor_object(actor).map_err(app)?,
        )?)),
        None => Ok(Found::NotFound),
    }
}

type PostRow = (
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Uuid,
    Option<Uuid>,
);

async fn note(ctx: Ctx, values: Values) -> anyhow::Result<Found<Value>> {
    let Some(post_id) = uuid(&values["post_id"]) else {
        return Ok(Found::NotFound);
    };
    let state = ctx.data();
    let row: Option<PostRow> = sqlx::query_as(
        "SELECT deleted_at, published_at, author_id, community_id FROM posts WHERE id = $1",
    )
    .bind(post_id)
    .fetch_optional(&state.db_pool)
    .await?;
    let Some((deleted_at, published_at, author_id, community_id)) = row else {
        return Ok(Found::NotFound);
    };
    if let Some(deleted_at) = deleted_at {
        return Ok(Found::Gone(Some(deleted_at)));
    }
    // A draft has not been published anywhere.
    if published_at.is_none() {
        return Ok(Found::NotFound);
    }

    let mut tx = state.db_pool.begin().await?;
    // Only posts from public and unlisted communities are served; a
    // personal post, in no community, always is.
    if let Some(community_id) = community_id
        && let Some(community) = find_community_by_id(&mut tx, community_id).await?
        && community.visibility == CommunityVisibility::Private
    {
        return Ok(Found::NotFound);
    }

    // The author's actor, created if they have none yet.
    let author = match Actor::find_by_user_id(&mut tx, author_id).await? {
        Some(actor) => actor,
        None => match find_user_by_id(&mut tx, author_id).await? {
            Some(user) => {
                tracing::info!(user = %user.id, "creating a missing actor for a post's author");
                create_actor_for_user(&mut tx, &user, &state.config).await?
            }
            None => return Ok(Found::NotFound),
        },
    };
    let note = create_note_from_post(
        &mut tx,
        post_id,
        &author,
        &state.config.domain,
        &state.config.r2_public_endpoint_url,
    )
    .await
    .map_err(app)?;
    tx.commit().await?;
    Ok(Found::Found(serde_json::to_value(note)?))
}

/// What a collection belongs to.
#[derive(Clone, Copy)]
enum Owner {
    User,
    Community,
}

impl Owner {
    /// The actor of the user or community `id`, if there is one.
    async fn actor(self, ctx: &Ctx, id: &str) -> anyhow::Result<Option<Uuid>> {
        let Some(id) = uuid(id) else {
            return Ok(None);
        };
        let query = match self {
            Self::User => "SELECT id FROM actors WHERE user_id = $1",
            Self::Community => "SELECT id FROM actors WHERE community_id = $1",
        };
        Ok(sqlx::query_scalar(query)
            .bind(id)
            .fetch_optional(&ctx.data().db_pool)
            .await?)
    }
}

/// Who follows a person or community: the count is shown and the members
/// are not.
fn followers(owner: Owner) -> Collection<AppState> {
    Collection::new(|_: Ctx, _: String, _: Option<String>| async move {
        // Hidden: there are no pages to ask for.
        Ok::<_, anyhow::Error>(None)
    })
    .count(move |ctx: Ctx, id: String| async move {
        let Some(actor) = owner.actor(&ctx, &id).await? else {
            return Ok::<_, anyhow::Error>(None);
        };
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM follows WHERE following_actor_id = $1")
                .bind(actor)
                .fetch_one(&ctx.data().db_pool)
                .await?;
        Ok(Some(u64::try_from(count).unwrap_or(0)))
    })
    .first_cursor(move |ctx: Ctx, id: String| async move {
        Ok::<_, anyhow::Error>(owner.actor(&ctx, &id).await?.map(|_| First::Hidden))
    })
}

/// An actor's outbox, which every actor has to have. What is posted is
/// delivered to followers as it is posted, and not listed here.
fn outbox(owner: Owner) -> Collection<AppState> {
    Collection::new(move |ctx: Ctx, id: String, _: Option<String>| async move {
        Ok::<_, anyhow::Error>(owner.actor(&ctx, &id).await?.map(|_| Page::default()))
    })
}

/// The person or community a name is the handle of: a user by login name,
/// then a community by slug, unless it is private.
async fn by_name(ctx: &Ctx, name: &str) -> anyhow::Result<Option<ActorRef>> {
    let mut tx = ctx.data().db_pool.begin().await?;
    if let Some(user) = find_user_by_login_name(&mut tx, name).await? {
        return Ok(Actor::find_by_user_id(&mut tx, user.id)
            .await?
            .map(|_| ActorRef::new("person", user.id.to_string())));
    }
    if let Some(community) = find_community_by_slug(&mut tx, name.to_owned()).await?
        && community.visibility != CommunityVisibility::Private
    {
        return Ok(Actor::find_by_community_id(&mut tx, community.id)
            .await?
            .map(|_| ActorRef::new("group", community.id.to_string())));
    }
    Ok(None)
}

async fn nodeinfo(ctx: Ctx) -> anyhow::Result<NodeInfo> {
    let db = &ctx.data().db_pool;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE deleted_at IS NULL")
        .fetch_one(db)
        .await?;
    let posts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM posts WHERE deleted_at IS NULL AND published_at IS NOT NULL",
    )
    .fetch_one(db)
    .await?;
    let mut nodeinfo = NodeInfo::new(Software {
        name: "oeee-cafe".to_owned(),
        // The crate's version is never bumped; the commit is what a release
        // is, and a local build has none.
        version: match crate::build_info::git_commit() {
            Some(commit) => format!(
                "{}+{}",
                env!("CARGO_PKG_VERSION"),
                &commit[..commit.len().min(12)]
            ),
            None => env!("CARGO_PKG_VERSION").to_owned(),
        },
        repository: Some("https://github.com/oeee-cafe/web".to_owned()),
        homepage: Some(format!("https://{}/", ctx.data().config.domain)),
    });
    nodeinfo.open_registrations = true;
    nodeinfo.usage.users_total = u64::try_from(users).ok();
    nodeinfo.usage.local_posts = u64::try_from(posts).ok();
    Ok(nodeinfo)
}

#[cfg(test)]
mod tests {
    /// Templates that overlap, or claim a path ojak serves, stop the build;
    /// this is where that would be found rather than at start-up.
    #[tokio::test]
    async fn the_federation_builds() {
        let client = crate::federation::client("oeee.cafe", &[]).unwrap();
        let fetcher = std::sync::Arc::new(ojak::fetch::Fetcher::new(
            client,
            ojak::sig::Scheme::DraftCavage,
        ));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres:///x")
            .unwrap();
        let kv = ojak_postgres::PostgresKvStore::new(pool);
        super::federation("oeee.cafe", fetcher, kv).expect("the federation builds");
    }
}
