//! Picking a community: the one query behind every place a person chooses or
//! looks up a community by what they type -- moving a post, saving a room
//! opened in none, the quick switcher, and the directory's search.
//!
//! They used to be four queries that disagreed: two escaped what was typed
//! and two let `%` and `_` through as wildcards, one searched descriptions
//! and three did not, each ranked its own way. What still differs between
//! them is *who may pick what*, and that is a [`ChoiceRule`]; everything
//! else -- the search, the ranking, the limit -- is the same here for all.

use crate::models::community::CommunityVisibility;
use crate::models::handle::LoginName;
use crate::models::tag::escape_like;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Which communities a picker offers. Every rule leaves out deleted ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceRule {
    /// Where an author may move a post: never private, never two-tone;
    /// unlisted only where they own or have posted; public ones with
    /// nothing posted only where they are the chooser's own.
    Move,
    /// Where a room opened in no community may be saved: public and not
    /// two-tone, because such a room let in whoever a public community's
    /// room would have. Nothing typed suggests the chooser's own only.
    LobbySave,
    /// The quick switcher: places to go, two-tone ones too. The chooser's
    /// private communities when they are a member, their unlisted ones,
    /// and public ones. Nothing typed lists the chooser's own only.
    Jump,
    /// The public directory's search: every public community, and nobody's
    /// own first -- it is the same list for everyone.
    Directory,
}

impl ChoiceRule {
    /// Matched inside the query's `CASE`s rather than interpolated, so the
    /// SQL stays checked at compile time (as `CommunitySort` does).
    fn as_sql(self) -> &'static str {
        match self {
            ChoiceRule::Move => "move",
            ChoiceRule::LobbySave => "lobby_save",
            ChoiceRule::Jump => "jump",
            ChoiceRule::Directory => "directory",
        }
    }
}

/// What to look for. `query` is taken literally; empty means nothing typed.
#[derive(Clone, Copy, Debug)]
pub struct ChoiceQuery<'a> {
    pub rule: ChoiceRule,
    /// Who is choosing; `None` for somebody signed out, who has no own.
    pub chooser: Option<Uuid>,
    pub query: &'a str,
    /// Left out of the list: the community a post is already in.
    pub exclude: Option<Uuid>,
    /// How many of the communities that are not the chooser's own to list,
    /// after skipping `offset` of them. The chooser's own are listed whole.
    /// Ask for one more than you show to learn whether there are more.
    pub limit: i64,
    pub offset: i64,
}

impl<'a> ChoiceQuery<'a> {
    pub fn new(rule: ChoiceRule, chooser: Option<Uuid>, query: &'a str, limit: i64) -> Self {
        ChoiceQuery {
            rule,
            chooser,
            query: query.trim(),
            exclude: None,
            limit,
            offset: 0,
        }
    }
}

/// One community a picker may offer, and what its row may say about it.
#[derive(Clone, Debug, Serialize)]
pub struct CommunityChoice {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub owner_login_name: LoginName,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Published, not deleted.
    pub posts_count: i64,
    pub is_member: bool,
    /// The chooser has posted there.
    pub has_participated: bool,
    pub is_owner: bool,
    /// Member, owner or has posted there, under any rule but `Directory`:
    /// listed first, and whole.
    pub is_mine: bool,
}

/// The communities `q.rule` lets `q.chooser` pick that match `q.query`: their
/// own first, then the rest; in each, a name or handle that *is* what was
/// typed, then one that starts with it, then ones they are a member of, then
/// the most recently posted in.
pub async fn choose_communities(
    tx: &mut Transaction<'_, Postgres>,
    q: &ChoiceQuery<'_>,
) -> Result<Vec<CommunityChoice>> {
    query(tx, q, None).await
}

/// Whether `q.rule` lets `chooser` pick `community` at all, typed or not:
/// what a form that sends an id is checked against.
pub async fn may_choose(
    tx: &mut Transaction<'_, Postgres>,
    rule: ChoiceRule,
    chooser: Option<Uuid>,
    community: Uuid,
) -> Result<bool> {
    let q = ChoiceQuery::new(rule, chooser, "", 1);
    Ok(!query(tx, &q, Some(community)).await?.is_empty())
}

async fn query(
    tx: &mut Transaction<'_, Postgres>,
    q: &ChoiceQuery<'_>,
    only: Option<Uuid>,
) -> Result<Vec<CommunityChoice>> {
    let escaped = escape_like(q.query);
    let rows = sqlx::query!(
        r#"
        WITH candidates AS (
            SELECT
                c.*,
                u.login_name AS owner_login_name,
                stats.posts_count,
                stats.last_posted_at,
                EXISTS(
                    SELECT 1 FROM community_members cm
                    WHERE cm.community_id = c.id AND cm.user_id = $2
                ) AS is_member,
                EXISTS(
                    SELECT 1 FROM posts p
                    WHERE p.community_id = c.id AND p.author_id = $2
                ) AS has_participated,
                c.owner_id IS NOT DISTINCT FROM $2 AND $2 IS NOT NULL AS is_owner,
                ($3 <> '' AND (c.name ILIKE $5 ESCAPE '\' OR c.slug ILIKE $5 ESCAPE '\')) AS is_exact,
                ($3 <> '' AND (c.name ILIKE $5 || '%' ESCAPE '\' OR c.slug ILIKE $5 || '%' ESCAPE '\')) AS is_prefix
            FROM communities c
            INNER JOIN users u ON u.id = c.owner_id
            CROSS JOIN LATERAL (
                SELECT COUNT(*) AS posts_count, MAX(p.published_at) AS last_posted_at
                FROM posts p
                WHERE p.community_id = c.id
                  AND p.published_at IS NOT NULL
                  AND p.deleted_at IS NULL
            ) stats
            WHERE c.deleted_at IS NULL
              AND c.id IS DISTINCT FROM $6
              AND ($7::uuid IS NULL OR c.id = $7)
              AND ($3 = ''
                   OR c.name ILIKE $4 ESCAPE '\'
                   OR c.slug ILIKE $4 ESCAPE '\'
                   OR c.description ILIKE $4 ESCAPE '\')
        ),
        mine AS (
            SELECT *,
                $1 <> 'directory' AND (is_member OR has_participated OR is_owner) AS is_mine
            FROM candidates
        ),
        offered AS (
            SELECT * FROM mine
            WHERE
              -- Two-tone communities take no drawings from elsewhere.
              NOT ($1 IN ('move', 'lobby_save')
                   AND background_color IS NOT NULL AND foreground_color IS NOT NULL)
              AND CASE visibility
                WHEN 'public' THEN
                  $1 <> 'move' OR is_mine OR last_posted_at IS NOT NULL
                WHEN 'unlisted' THEN
                  $1 IN ('move', 'jump') AND is_mine
                ELSE
                  $1 = 'jump' AND is_member
              END
              -- Nothing typed suggests only the chooser's own, where a list
              -- of everybody's would bury them. A check of one id is not a list.
              AND ($7::uuid IS NOT NULL OR $3 <> '' OR $1 NOT IN ('lobby_save', 'jump') OR is_mine)
        ),
        ranked AS (
            SELECT *, ROW_NUMBER() OVER (
                PARTITION BY is_mine
                ORDER BY is_exact DESC, is_prefix DESC, is_member DESC,
                         last_posted_at DESC NULLS LAST, name
            ) AS rn
            FROM offered
        )
        SELECT
            id AS "id!",
            owner_id AS "owner_id!",
            owner_login_name AS "owner_login_name!",
            name AS "name!",
            slug AS "slug!",
            description AS "description!",
            visibility AS "visibility!: CommunityVisibility",
            background_color,
            foreground_color,
            created_at AS "created_at!",
            updated_at AS "updated_at!",
            posts_count AS "posts_count!",
            is_member AS "is_member!",
            has_participated AS "has_participated!",
            is_owner AS "is_owner!",
            is_mine AS "is_mine!"
        FROM ranked
        WHERE is_mine OR (rn > $9 AND rn <= $9 + $8)
        ORDER BY is_mine DESC, rn
        "#,
        q.rule.as_sql(),
        q.chooser,
        q.query,
        format!("%{escaped}%"),
        escaped,
        q.exclude,
        only,
        q.limit,
        q.offset,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| CommunityChoice {
            id: row.id,
            owner_id: row.owner_id,
            owner_login_name: row.owner_login_name.into(),
            name: row.name,
            slug: row.slug,
            description: row.description,
            visibility: row.visibility,
            background_color: row.background_color,
            foreground_color: row.foreground_color,
            created_at: row.created_at,
            updated_at: row.updated_at,
            posts_count: row.posts_count,
            is_member: row.is_member,
            has_participated: row.has_participated,
            is_owner: row.is_owner,
            is_mine: row.is_mine,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    /// One of each kind of community, named `{kind}_{tag}`, each with a
    /// drawing by `other` unless it is `empty`. `me` owns the unlisted one
    /// and is a member of the private one; `stranger` is in none of them.
    struct World {
        tag: String,
        me: Uuid,
        stranger: Uuid,
        ids: HashMap<&'static str, Uuid>,
    }

    async fn world(tx: &mut Transaction<'static, Postgres>) -> World {
        let tag = Uuid::new_v4().simple().to_string()[..12].to_string();
        let mut users = Vec::new();
        for who in ["me", "other", "stranger"] {
            users.push(
                sqlx::query_scalar!(
                    "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
                    format!("{who}_{tag}")
                )
                .fetch_one(&mut **tx)
                .await
                .unwrap(),
            );
        }
        let (me, other, stranger) = (users[0], users[1], users[2]);
        let mut ids = HashMap::new();
        for (kind, owner, visibility, two_tone, posted) in [
            ("mine_unlisted", me, "unlisted", false, true),
            ("their_unlisted", other, "unlisted", false, true),
            ("private", other, "private", false, true),
            ("twotone", other, "public", true, true),
            ("pub_a", other, "public", false, true),
            ("pub_b", other, "public", false, true),
            ("pct_c", other, "public", false, true),
            ("empty", other, "public", false, false),
        ] {
            let colour = two_tone.then_some("#000000");
            let id = sqlx::query_scalar!(
                r#"INSERT INTO communities
                   (owner_id, name, description, visibility, slug, foreground_color, background_color)
                   VALUES ($1, $2, '', $3::text::community_visibility, $2, $4, $4)
                   RETURNING id"#,
                owner,
                format!("{kind}_{tag}"),
                visibility,
                colour,
            )
            .fetch_one(&mut **tx)
            .await
            .unwrap();
            if posted {
                let image = sqlx::query_scalar!(
                    "INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
                     VALUES (1, 1, '1 second', 1, $1, 'neo') RETURNING id",
                    format!("{kind}_{tag}.png")
                )
                .fetch_one(&mut **tx)
                .await
                .unwrap();
                sqlx::query!(
                    "INSERT INTO posts (author_id, community_id, image_id, published_at)
                     VALUES ($1, $2, $3, now())",
                    other,
                    id,
                    image
                )
                .execute(&mut **tx)
                .await
                .unwrap();
            }
            ids.insert(kind, id);
        }
        sqlx::query!(
            "INSERT INTO community_members (community_id, user_id, role) VALUES ($1, $2, 'member')",
            ids["private"],
            me
        )
        .execute(&mut **tx)
        .await
        .unwrap();
        World {
            tag,
            me,
            stranger,
            ids,
        }
    }

    impl World {
        async fn names(
            &self,
            tx: &mut Transaction<'static, Postgres>,
            rule: ChoiceRule,
            chooser: Uuid,
            query: &str,
        ) -> Vec<String> {
            let query = if query.is_empty() {
                String::new()
            } else {
                query.replace("TAG", &self.tag)
            };
            let mut names: Vec<String> =
                choose_communities(tx, &ChoiceQuery::new(rule, Some(chooser), &query, 50))
                    .await
                    .unwrap()
                    .into_iter()
                    .filter_map(|c| {
                        c.name
                            .strip_suffix(&format!("_{}", self.tag))
                            .map(String::from)
                    })
                    .collect();
            names.sort();
            names
        }
    }

    #[tokio::test]
    async fn each_rule_offers_what_it_should() {
        let Some(mut tx) = tx().await else { return };
        let w = world(&mut tx).await;
        let (me, stranger) = (w.me, w.stranger);
        use ChoiceRule::*;

        // Moving a post: their unlisted one and public ones with drawings.
        assert_eq!(
            w.names(&mut tx, Move, me, "TAG").await,
            ["mine_unlisted", "pct_c", "pub_a", "pub_b"]
        );
        // Saving a lobby room: public only, nothing typed suggests their own,
        // and they have none that is public.
        assert_eq!(
            w.names(&mut tx, LobbySave, me, "TAG").await,
            ["empty", "pct_c", "pub_a", "pub_b"]
        );
        assert!(w.names(&mut tx, LobbySave, me, "").await.is_empty());
        // The switcher: their private and unlisted ones, two-tone too.
        assert_eq!(
            w.names(&mut tx, Jump, me, "TAG").await,
            [
                "empty",
                "mine_unlisted",
                "pct_c",
                "private",
                "pub_a",
                "pub_b",
                "twotone"
            ]
        );
        assert_eq!(
            w.names(&mut tx, Jump, me, "").await,
            ["mine_unlisted", "private"]
        );
        assert_eq!(
            w.names(&mut tx, Jump, stranger, "TAG").await,
            ["empty", "pct_c", "pub_a", "pub_b", "twotone"]
        );
        // The directory: every public community, the same for anybody.
        assert_eq!(
            w.names(&mut tx, Directory, me, "TAG").await,
            w.names(&mut tx, Directory, stranger, "TAG").await
        );

        // `_` and `%` are characters, not wildcards.
        assert_eq!(w.names(&mut tx, Directory, me, "pub_").await.len(), 2);
        assert!(w.names(&mut tx, Directory, me, "pct%").await.is_empty());

        // A check of one id is not a list: no typing needed.
        assert!(
            may_choose(&mut tx, LobbySave, Some(stranger), w.ids["pub_a"])
                .await
                .unwrap()
        );
        assert!(
            !may_choose(&mut tx, LobbySave, Some(me), w.ids["mine_unlisted"])
                .await
                .unwrap()
        );
        assert!(!may_choose(&mut tx, LobbySave, Some(me), w.ids["twotone"])
            .await
            .unwrap());
        assert!(may_choose(&mut tx, Jump, Some(me), w.ids["private"])
            .await
            .unwrap());
        assert!(!may_choose(&mut tx, Jump, Some(stranger), w.ids["private"])
            .await
            .unwrap());
        assert!(!may_choose(&mut tx, Jump, None, w.ids["private"])
            .await
            .unwrap());
    }

    /// The chooser's own come first and whole; the rest are cut at the limit,
    /// after the offset; an exact name outranks one that only contains it.
    #[tokio::test]
    async fn own_first_then_the_rest_cut_at_the_limit() {
        let Some(mut tx) = tx().await else { return };
        let w = world(&mut tx).await;
        let query = w.tag.clone();

        let cut = choose_communities(
            &mut tx,
            &ChoiceQuery::new(ChoiceRule::Move, Some(w.me), &query, 2),
        )
        .await
        .unwrap();
        assert!(cut[0].is_mine);
        assert_eq!(cut.iter().filter(|c| c.is_mine).count(), 1);
        assert_eq!(cut.iter().filter(|c| !c.is_mine).count(), 2);

        let page = |offset| ChoiceQuery {
            offset,
            ..ChoiceQuery::new(ChoiceRule::Directory, None, &query, 2)
        };
        let first = choose_communities(&mut tx, &page(0)).await.unwrap();
        let second = choose_communities(&mut tx, &page(2)).await.unwrap();
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|a| second.iter().all(|b| a.id != b.id)));

        let exact = format!("pub_b_{}", w.tag);
        let ranked = choose_communities(
            &mut tx,
            &ChoiceQuery::new(ChoiceRule::Directory, None, &exact.to_uppercase(), 10),
        )
        .await
        .unwrap();
        assert_eq!(ranked[0].name, exact);
    }
}
