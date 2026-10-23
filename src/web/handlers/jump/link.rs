//! An address of the site's own, pasted into the quick switcher.
//!
//! Somebody holding a link -- a room a friend sent, a post from a chat, a
//! profile -- can paste it and be offered the one row it names, with its name
//! rather than its uuid. Only what the reader could open by following the
//! link themselves comes back, and by the rule that page itself keeps: a
//! private community's things for its members, a draft for its author, a room
//! for whoever `viewer_may_enter` lets in. Anything else is no row at all, so
//! the switcher never says what a link the reader may not open is called.
//!
//! The address is read for what it names, not for which of its pages it is:
//! a post's replay is the post, a profile's guestbook is the profile, and the
//! row goes to that thing's canonical page.

use data_encoding::BASE64URL_NOPAD;
use serde::Serialize;
use sqlx::{Pool, Postgres, Transaction};
use tower_sessions::Session;
use url::Url;
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::community::{
    find_community_by_id, find_community_by_slug, is_user_member, Community, CommunityVisibility,
};
use crate::models::post::find_post_by_id;
use crate::models::post::post_page_path;
use crate::models::tag::{find_tag_by_name, normalize_tag};
use crate::models::user::{find_user_by_id, find_user_by_login_name, User};
use crate::web::handlers::collaborate::{db as rooms, guest};

/// What an address names, read off its path alone.
#[derive(Debug, PartialEq)]
pub(super) enum Permalink {
    /// `/@{name}` and the pages under it: a person, or a community, which
    /// share the one namespace.
    Handle(String),
    /// `/ap/users/{id}`.
    Person(Uuid),
    /// `/communities/{id}` and `/ap/communities/{id}`.
    Community(Uuid),
    /// `/@{name}/{id}`, `/posts/{id}` and `/ap/posts/{id}`.
    Post(Uuid),
    /// `/collaborate/{id}`.
    Room(Uuid),
    /// `/tags/{name}`.
    Tag(String),
}

/// The thing `q` is an address of, when it is one of this site's: a whole
/// URL on `domain`, the same without its scheme, or a path from the root.
/// Anything else, a word typed included, is not an address.
pub(super) fn parse(q: &str, domain: &str) -> Option<Permalink> {
    let base = Url::parse(&format!("https://{domain}/")).ok()?;
    let url = if q.starts_with('/') && !q.starts_with("//") {
        base.join(q).ok()?
    } else if q.contains("://") {
        Url::parse(q).ok()?
    } else {
        Url::parse(&format!("https://{q}")).ok()?
    };
    if !matches!(url.scheme(), "http" | "https") || !same_host(&url, domain) {
        return None;
    }
    let segments: Vec<String> = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            urlencoding::decode(segment)
                .map(|decoded| decoded.into_owned())
                .unwrap_or_else(|_| segment.to_string())
        })
        .collect();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    match segments.as_slice() {
        [handle, rest @ ..] if handle.starts_with('@') => {
            let name = &handle[1..];
            if name.is_empty() {
                return None;
            }
            match rest.first().and_then(|segment| id(segment)) {
                Some(post) => Some(Permalink::Post(post)),
                None => Some(Permalink::Handle(name.to_string())),
            }
        }
        ["posts", post, ..] => id(post).map(Permalink::Post),
        ["communities", community, ..] => match community.strip_prefix('@') {
            Some("") => None,
            Some(slug) => Some(Permalink::Handle(slug.to_string())),
            None => id(community).map(Permalink::Community),
        },
        ["collaborate", room, ..] => Uuid::parse_str(room).ok().map(Permalink::Room),
        ["tags", tag, ..] => Some(Permalink::Tag((*tag).to_string())),
        ["ap", "users", person, ..] => Uuid::parse_str(person).ok().map(Permalink::Person),
        ["ap", "posts", post, ..] => Uuid::parse_str(post).ok().map(Permalink::Post),
        ["ap", "communities", community, ..] => {
            Uuid::parse_str(community).ok().map(Permalink::Community)
        }
        _ => None,
    }
}

/// `domain` is the site's host with its port when it has one
/// (`localhost:3000`), as the config spells it.
fn same_host(url: &Url, domain: &str) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    host.eq_ignore_ascii_case(domain)
}

/// An id as the pages take it: a uuid, or the base64 of one that older links
/// carry (`parse_id_with_legacy_support`).
fn id(segment: &str) -> Option<Uuid> {
    if let Ok(uuid) = Uuid::parse_str(segment) {
        return Some(uuid);
    }
    let bytes = BASE64URL_NOPAD.decode(segment.as_bytes()).ok()?;
    Uuid::from_slice(&bytes).ok()
}

/// The row a link becomes, drawn by jump_results.jinja.
#[derive(Debug, Serialize)]
pub(super) struct JumpLink {
    /// `person`, `community`, `post`, `room` or `tag`.
    kind: &'static str,
    href: String,
    /// Absent for an untitled post or room, which the template names.
    name: Option<String>,
    /// The handle of a person or of a post's author.
    login_name: Option<String>,
    /// A community's handle, the community a post or room is in, a tag's
    /// count of drawings.
    detail: Option<String>,
}

/// Who is asking, as each page would see them.
pub(super) struct Reader<'a> {
    pub user: Option<&'a User>,
    pub session: &'a Session,
    pub ip: Option<String>,
}

/// The row `link` names, if it names something the reader may open.
pub(super) async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    db: &Pool<Postgres>,
    link: Permalink,
    reader: &Reader<'_>,
) -> Result<Option<JumpLink>, AppError> {
    match link {
        Permalink::Handle(name) => {
            if let Some(user) = find_user_by_login_name(tx, &name).await? {
                return Ok(person(user));
            }
            match find_community_by_slug(tx, name).await? {
                Some(found) => community(tx, found, reader).await,
                None => Ok(None),
            }
        }
        Permalink::Person(id) => Ok(find_user_by_id(tx, id).await?.and_then(person)),
        Permalink::Community(id) => match find_community_by_id(tx, id).await? {
            Some(found) => community(tx, found, reader).await,
            None => Ok(None),
        },
        Permalink::Post(id) => post(tx, id, reader).await,
        Permalink::Room(id) => room(tx, db, id, reader).await,
        Permalink::Tag(name) => {
            let name = normalize_tag(&name);
            if name.is_empty() {
                return Ok(None);
            }
            Ok(find_tag_by_name(tx, &name).await?.map(|tag| JumpLink {
                kind: "tag",
                href: format!("/tags/{}", urlencoding::encode(&tag.name)),
                name: Some(tag.display_name),
                login_name: None,
                detail: Some(tag.post_count.to_string()),
            }))
        }
    }
}

fn person(user: User) -> Option<JumpLink> {
    if user.deleted_at.is_some() {
        return None;
    }
    Some(JumpLink {
        kind: "person",
        href: format!("/@{}", user.login_name),
        name: Some(user.display_name),
        login_name: Some(user.login_name.to_string()),
        detail: None,
    })
}

/// A private community is its members' (`render_community_page`); a public or
/// unlisted one is anybody's who has the link.
async fn may_see_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    visibility: CommunityVisibility,
    reader: &Reader<'_>,
) -> Result<bool, AppError> {
    if visibility != CommunityVisibility::Private {
        return Ok(true);
    }
    match reader.user {
        Some(user) => Ok(is_user_member(tx, user.id, community_id).await?),
        None => Ok(false),
    }
}

async fn community(
    tx: &mut Transaction<'_, Postgres>,
    found: Community,
    reader: &Reader<'_>,
) -> Result<Option<JumpLink>, AppError> {
    if !may_see_community(tx, found.id, found.visibility, reader).await? {
        return Ok(None);
    }
    Ok(Some(JumpLink {
        kind: "community",
        href: format!("/@{}", found.slug),
        detail: Some(format!("@{}", found.slug)),
        name: Some(found.name),
        login_name: None,
    }))
}

/// A post its page would show the reader: not deleted, published unless the
/// reader wrote it, and in a private community only for its members.
async fn post(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    reader: &Reader<'_>,
) -> Result<Option<JumpLink>, AppError> {
    let Some(found) = find_post_by_id(tx, id).await? else {
        return Ok(None);
    };
    let field = |key: &str| found.get(key).and_then(|value| value.clone());
    let (Some(login_name), Some(author_id)) = (field("login_name"), field("author_id")) else {
        return Ok(None);
    };
    let is_author = reader
        .user
        .is_some_and(|user| user.id.to_string() == author_id);
    if field("published_at").is_none() && !is_author {
        return Ok(None);
    }
    if let Some(community_id) = field("community_id").and_then(|id| Uuid::parse_str(&id).ok()) {
        let Some(community) = find_community_by_id(tx, community_id).await? else {
            return Ok(None);
        };
        if !may_see_community(tx, community.id, community.visibility, reader).await? {
            return Ok(None);
        }
    }
    let community_slug = field("community_slug");
    Ok(Some(JumpLink {
        kind: "post",
        href: post_page_path(&login_name, community_slug.as_deref(), id),
        name: field("title").filter(|title| !title.trim().is_empty()),
        login_name: Some(login_name),
        detail: field("community_name"),
    }))
}

/// An open room the reader would be let into; an ended one is the post it
/// was saved as, if it was.
async fn room(
    tx: &mut Transaction<'_, Postgres>,
    db: &Pool<Postgres>,
    id: Uuid,
    reader: &Reader<'_>,
) -> Result<Option<JumpLink>, AppError> {
    let Some(found) = sqlx::query!(
        r#"
        SELECT cs.title, cs.ended_at, cs.saved_post_id, c.name AS "community_name?"
        FROM collaborative_sessions cs
        LEFT JOIN communities c ON cs.community_id = c.id
        WHERE cs.id = $1
        "#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    if found.ended_at.is_some() {
        return match found.saved_post_id {
            Some(post_id) => post(tx, post_id, reader).await,
            None => Ok(None),
        };
    }
    let who = guest::current(reader.user, reader.session, db).await?;
    if !rooms::viewer_may_enter(db, id, who.as_ref(), reader.ip.as_deref()).await? {
        return Ok(None);
    }
    Ok(Some(JumpLink {
        kind: "room",
        href: format!("/collaborate/{id}"),
        name: found.title.filter(|title| !title.trim().is_empty()),
        login_name: None,
        detail: found.community_name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN: &str = "oeee.cafe";
    const ID: &str = "0199a0e2-7c1d-7b3a-9f00-0123456789ab";

    fn uuid() -> Uuid {
        Uuid::parse_str(ID).unwrap()
    }

    fn parsed(q: &str) -> Option<Permalink> {
        parse(q, DOMAIN)
    }

    #[test]
    fn a_whole_url_a_bare_host_and_a_path_all_name_the_same_thing() {
        for q in [
            "https://oeee.cafe/@tandemaus",
            "http://OEEE.cafe/@tandemaus?tab=posts#top",
            "oeee.cafe/@tandemaus/",
            "/@tandemaus",
            "https://oeee.cafe/@tandemaus/guestbook",
        ] {
            assert_eq!(
                parsed(q),
                Some(Permalink::Handle("tandemaus".into())),
                "{q}"
            );
        }
    }

    #[test]
    fn each_kind_of_page_is_read_for_what_it_names() {
        let cases = [
            (
                format!("https://oeee.cafe/@club/{ID}"),
                Permalink::Post(uuid()),
            ),
            (
                format!("https://oeee.cafe/@neo/{ID}/replay"),
                Permalink::Post(uuid()),
            ),
            (format!("/posts/{ID}"), Permalink::Post(uuid())),
            (format!("/ap/posts/{ID}"), Permalink::Post(uuid())),
            (format!("/ap/users/{ID}"), Permalink::Person(uuid())),
            (format!("/communities/{ID}"), Permalink::Community(uuid())),
            (
                format!("/ap/communities/{ID}"),
                Permalink::Community(uuid()),
            ),
            (
                "/communities/@club/members".into(),
                Permalink::Handle("club".into()),
            ),
            (
                format!("https://oeee.cafe/collaborate/{ID}?watch=1"),
                Permalink::Room(uuid()),
            ),
            (
                "/tags/%EA%B7%B8%EB%A6%BC".into(),
                Permalink::Tag("그림".into()),
            ),
        ];
        for (q, expected) in cases {
            assert_eq!(parsed(&q), Some(expected), "{q}");
        }
    }

    #[test]
    fn an_older_links_base64_id_is_the_same_post() {
        let short = BASE64URL_NOPAD.encode(uuid().as_bytes());
        assert_eq!(
            parsed(&format!("/@neo/{short}")),
            Some(Permalink::Post(uuid()))
        );
    }

    #[test]
    fn words_and_other_sites_are_not_links() {
        for q in [
            "tandemaus",
            "@tandemaus",
            "drawing club",
            "https://example.com/@tandemaus",
            "oeee.cafe.example.com/@tandemaus",
            "//example.com/@tandemaus",
            "https://oeee.cafe/",
            "https://oeee.cafe/collaborate",
            "https://oeee.cafe/collaborate/not-a-room",
            "/posts/drafts",
            "/@",
            "ftp://oeee.cafe/@tandemaus",
        ] {
            assert_eq!(parsed(q), None, "{q}");
        }
    }

    #[test]
    fn a_port_is_part_of_the_host() {
        assert_eq!(
            parse("http://localhost:3000/@neo", "localhost:3000"),
            Some(Permalink::Handle("neo".into()))
        );
        assert_eq!(parse("http://localhost/@neo", "localhost:3000"), None);
    }

    mod access {
        use super::super::*;
        use crate::models::people::fixtures::{person, post, tag, tx};
        use std::sync::Arc;

        async fn reader(tx: &mut Transaction<'_, Postgres>, id: Option<Uuid>) -> Option<User> {
            match id {
                Some(id) => find_user_by_id(tx, id).await.unwrap(),
                None => None,
            }
        }

        /// The href `link` resolves to for `who`, or None if nothing does.
        async fn opens(
            tx: &mut Transaction<'_, Postgres>,
            link: Permalink,
            who: Option<Uuid>,
        ) -> Option<String> {
            let url = std::env::var("DATABASE_URL").unwrap();
            let db = sqlx::PgPool::connect(&url).await.unwrap();
            let user = reader(tx, who).await;
            let session =
                Session::new(None, Arc::new(tower_sessions::MemoryStore::default()), None);
            let reader = Reader {
                user: user.as_ref(),
                session: &session,
                ip: None,
            };
            resolve(tx, &db, link, &reader)
                .await
                .unwrap()
                .map(|found| found.href)
        }

        async fn community(
            tx: &mut Transaction<'_, Postgres>,
            owner: Uuid,
            slug: &str,
            visibility: &str,
        ) -> Uuid {
            let id = sqlx::query_scalar(
                "INSERT INTO communities (owner_id, name, slug, description, visibility)
                 VALUES ($1, $2, $2, '', $3::community_visibility) RETURNING id",
            )
            .bind(owner)
            .bind(slug)
            .bind(visibility)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
            sqlx::query("INSERT INTO community_members (community_id, user_id, role) VALUES ($1, $2, 'owner')")
                .bind(id)
                .bind(owner)
                .execute(&mut **tx)
                .await
                .unwrap();
            id
        }

        /// A private community and what is drawn in it are its members' to
        /// find; to anybody else, signed in or not, the link names nothing.
        #[tokio::test]
        async fn a_private_communitys_link_opens_only_for_its_members() {
            let Some(mut tx) = tx().await else { return };
            let t = tag();
            let (member, _) = person(&mut tx, &format!("m{t}"), "Member").await;
            let (outsider, _) = person(&mut tx, &format!("o{t}"), "Outsider").await;
            let slug = format!("c{t}");
            let club = community(&mut tx, member, &slug, "private").await;
            let drawing = post(&mut tx, member).await;
            sqlx::query("UPDATE posts SET community_id = $1 WHERE id = $2")
                .bind(club)
                .bind(drawing)
                .execute(&mut *tx)
                .await
                .unwrap();

            let page = format!("/@{slug}/{drawing}");
            assert_eq!(
                opens(&mut tx, Permalink::Post(drawing), Some(member)).await,
                Some(page)
            );
            assert_eq!(
                opens(&mut tx, Permalink::Post(drawing), Some(outsider)).await,
                None
            );
            assert_eq!(opens(&mut tx, Permalink::Post(drawing), None).await, None);
            assert_eq!(
                opens(&mut tx, Permalink::Handle(slug.clone()), Some(member)).await,
                Some(format!("/@{slug}"))
            );
            assert_eq!(
                opens(&mut tx, Permalink::Handle(slug.clone()), Some(outsider)).await,
                None
            );
            assert_eq!(opens(&mut tx, Permalink::Community(club), None).await, None);
        }

        /// An unlisted community is anybody's who holds the link, as its
        /// page is.
        #[tokio::test]
        async fn an_unlisted_community_opens_for_anyone_with_the_link() {
            let Some(mut tx) = tx().await else { return };
            let t = tag();
            let (owner, _) = person(&mut tx, &format!("u{t}"), "Owner").await;
            let slug = format!("c{t}");
            let club = community(&mut tx, owner, &slug, "unlisted").await;
            assert_eq!(
                opens(&mut tx, Permalink::Community(club), None).await,
                Some(format!("/@{slug}"))
            );
        }

        /// A draft is its author's; a deleted account is nobody's.
        #[tokio::test]
        async fn a_draft_opens_for_its_author_and_a_deleted_account_for_nobody() {
            let Some(mut tx) = tx().await else { return };
            let t = tag();
            let login = format!("a{t}");
            let (author, _) = person(&mut tx, &login, "Author").await;
            let (other, _) = person(&mut tx, &format!("b{t}"), "Other").await;
            let draft = post(&mut tx, author).await;
            sqlx::query("UPDATE posts SET published_at = NULL WHERE id = $1")
                .bind(draft)
                .execute(&mut *tx)
                .await
                .unwrap();
            assert_eq!(
                opens(&mut tx, Permalink::Post(draft), Some(author)).await,
                Some(format!("/@{login}/{draft}"))
            );
            assert_eq!(
                opens(&mut tx, Permalink::Post(draft), Some(other)).await,
                None
            );

            assert_eq!(
                opens(&mut tx, Permalink::Handle(login.clone()), None).await,
                Some(format!("/@{login}"))
            );
            sqlx::query("UPDATE users SET deleted_at = now() WHERE id = $1")
                .bind(author)
                .execute(&mut *tx)
                .await
                .unwrap();
            assert_eq!(opens(&mut tx, Permalink::Handle(login), None).await, None);
            assert_eq!(opens(&mut tx, Permalink::Person(author), None).await, None);
        }
    }
}
