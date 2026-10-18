//! Changing a post: its text, and which community it is in.

use crate::models::handle::LoginName;
use anyhow::Result;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use crate::models::community::CommunityVisibility;

pub async fn edit_post(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    title: String,
    content: String,
    is_sensitive: bool,
    allow_relay: bool,
    allow_replay: bool,
) -> Result<()> {
    let q = query!(
        "
            UPDATE posts
            SET
                title = $1,
                content = $2,
                is_sensitive = $3,
                allow_relay = $4,
                allow_replay = $5
            WHERE id = $6
        ",
        title,
        content,
        is_sensitive,
        allow_relay,
        allow_replay,
        id
    );
    q.execute(&mut **tx).await?;
    Ok(())
}

pub async fn edit_post_community(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    new_community_id: Option<Uuid>,
) -> Result<()> {
    // Update the post's community (can be None for personal posts)
    // Note: We don't use recursive CTE anymore since threaded posts cannot be moved
    let q = query!(
        r#"
            UPDATE posts
            SET community_id = $1
            WHERE id = $2
        "#,
        new_community_id,
        id
    );
    q.execute(&mut **tx).await?;
    Ok(())
}

/// Check if a post can be moved to a different community
/// Returns true if the post is movable (not part of a thread and not in private/two-tone community)
pub async fn is_post_movable(tx: &mut Transaction<'_, Postgres>, post_id: Uuid) -> Result<bool> {
    // Check if post has a parent (is a reply)
    let has_parent = query!(r#"SELECT parent_post_id FROM posts WHERE id = $1"#, post_id)
        .fetch_one(&mut **tx)
        .await?
        .parent_post_id
        .is_some();

    if has_parent {
        return Ok(false);
    }

    // Check if post has children (is a parent)
    let has_children = query!(
        r#"SELECT EXISTS(SELECT 1 FROM posts WHERE parent_post_id = $1) as "has_children!""#,
        post_id
    )
    .fetch_one(&mut **tx)
    .await?
    .has_children;

    if has_children {
        return Ok(false);
    }

    // Check if post is in a private or two-tone community
    let community_info = query!(
        r#"
        SELECT
            c.visibility::text as visibility,
            c.background_color,
            c.foreground_color
        FROM posts p
        LEFT JOIN communities c ON p.community_id = c.id
        WHERE p.id = $1
        "#,
        post_id
    )
    .fetch_one(&mut **tx)
    .await?;

    // If in a community, check restrictions
    if let Some(visibility_str) = community_info.visibility {
        // Cannot move from private communities
        if visibility_str == "private" {
            return Ok(false);
        }

        // Cannot move from two-tone communities (both colors set)
        if community_info.background_color.is_some() && community_info.foreground_color.is_some() {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Whether an existing post may be moved into `community_id`.
///
/// The move form (`post_edit_community`) only offers communities that pass
/// this, but the id arrives from a form field, so the handler has to ask
/// again. A community that is gone or two-tone is out, as it is for the form:
/// a two-tone community's drawings are made with its two colours, and one
/// drawn with any others does not belong there. A private one is out even for
/// a member. Publishing lets members in, but a move federates nothing, so a
/// post that has already gone out would stay up on every server that has it
/// while its page here disappeared behind the community's membership.
/// Public and unlisted are open, as they are to draw for (`may_draw_in`).
pub async fn may_move_post_into(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<bool> {
    let community = query!(
        r#"
        SELECT
            visibility as "visibility: CommunityVisibility",
            background_color,
            foreground_color
        FROM communities
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        community_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(community.is_some_and(|c| {
        c.visibility != CommunityVisibility::Private
            && !(c.background_color.is_some() && c.foreground_color.is_some())
    }))
}

/// One community the caller may put a drawing into, carrying the two signals a
/// picker needs to rank it: whether they are a member, and whether they have
/// posted there before.
#[derive(Clone, Debug)]
pub struct PostableCommunity {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub owner_login_name: LoginName,
    pub has_participated: bool,
    pub is_member: bool,
}

/// Get list of communities that a post can be moved to
/// Includes:
/// - All public communities
/// - Unlisted communities where the user has posted before
/// - Private communities where the user is a member
///
/// Excludes two-tone communities (both colors set)
pub async fn get_movable_communities(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<PostableCommunity>> {
    let communities = query!(
        r#"
        SELECT
            c.id,
            c.name,
            c.slug,
            c.description,
            c.visibility as "visibility: CommunityVisibility",
            u.login_name as "owner_login_name!",
            EXISTS(
                SELECT 1 FROM posts p
                WHERE p.community_id = c.id AND p.author_id = $1
            ) as "has_participated!",
            EXISTS(
                SELECT 1 FROM community_members cm
                WHERE cm.community_id = c.id AND cm.user_id = $1
            ) as "is_member!"
        FROM communities c
        INNER JOIN users u ON c.owner_id = u.id
        WHERE c.deleted_at IS NULL
          AND NOT (c.background_color IS NOT NULL AND c.foreground_color IS NOT NULL)
          AND (
            -- All public communities
            c.visibility = 'public'
            OR (
              -- Unlisted communities where user has posted
              c.visibility = 'unlisted'
              AND EXISTS(
                SELECT 1 FROM posts p
                WHERE p.community_id = c.id AND p.author_id = $1
              )
            )
            OR (
              -- Private communities where user is a member
              c.visibility = 'private'
              AND EXISTS(
                SELECT 1 FROM community_members cm
                WHERE cm.community_id = c.id AND cm.user_id = $1
              )
            )
          )
        ORDER BY c.name ASC
        "#,
        user_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| PostableCommunity {
        id: row.id,
        name: row.name,
        slug: row.slug,
        description: row.description,
        visibility: row.visibility,
        owner_login_name: row.owner_login_name.into(),
        has_participated: row.has_participated,
        is_member: row.is_member,
    })
    .collect();

    Ok(communities)
}

/// A community the move page offers, with what its row says about it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MoveDestination {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub visibility: CommunityVisibility,
    pub owner_login_name: LoginName,
    pub posts_count: i64,
    /// Owned by the mover or posted in by them: listed whole, ahead of the
    /// rest, and the only way an unlisted community is offered at all.
    pub is_known: bool,
    pub has_participated: bool,
}

/// How many communities the move page lists beyond the mover's own, most
/// active first. The rest are a search away.
pub const MOVE_OTHERS_LIMIT: i64 = 30;

/// Where the author may move a post, narrowed by `query` when there is one.
///
/// The same rule `may_move_post_into` enforces -- never private, never
/// two-tone -- plus: unlisted only where the author owns it or has posted, and
/// a public community nobody has posted in only when it is one of those. The
/// mover's own are returned whole; of the others at most `others_limit`, the
/// ones whose name starts with the query first, then the most recently active.
/// One more than the limit is fetched, so the caller can tell there are more.
pub async fn move_destinations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    exclude: Option<Uuid>,
    query: &str,
    others_limit: i64,
) -> Result<Vec<MoveDestination>> {
    let query = query.trim();
    let escaped = crate::models::tag::escape_like(query);
    let rows = sqlx::query!(
        r#"
        WITH candidates AS (
            SELECT
                c.id,
                c.name,
                c.slug,
                c.visibility,
                u.login_name,
                stats.posts_count,
                stats.last_posted_at,
                EXISTS(
                    SELECT 1 FROM posts p
                    WHERE p.community_id = c.id AND p.author_id = $1
                ) AS has_participated,
                c.owner_id = $1 AS is_owner,
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
              AND c.visibility <> 'private'
              AND NOT (c.background_color IS NOT NULL AND c.foreground_color IS NOT NULL)
              AND c.id IS DISTINCT FROM $2
              AND ($3 = ''
                   OR c.name ILIKE $4 ESCAPE '\'
                   OR c.slug ILIKE $4 ESCAPE '\'
                   OR c.description ILIKE $4 ESCAPE '\')
        ),
        offered AS (
            SELECT *, (has_participated OR is_owner) AS is_known
            FROM candidates
            WHERE has_participated OR is_owner
               OR (visibility = 'public' AND last_posted_at IS NOT NULL)
        ),
        ranked AS (
            SELECT *, ROW_NUMBER() OVER (
                PARTITION BY is_known
                ORDER BY is_prefix DESC, last_posted_at DESC NULLS LAST, name
            ) AS rn
            FROM offered
        )
        SELECT
            id AS "id!",
            name AS "name!",
            slug AS "slug!",
            visibility AS "visibility!: CommunityVisibility",
            login_name AS "owner_login_name!",
            posts_count AS "posts_count!",
            is_known AS "is_known!",
            has_participated AS "has_participated!"
        FROM ranked
        WHERE is_known OR rn <= $6
        ORDER BY is_known DESC, rn
        "#,
        user_id,
        exclude,
        query,
        format!("%{escaped}%"),
        escaped,
        others_limit + 1,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| MoveDestination {
            id: row.id,
            name: row.name,
            slug: row.slug,
            visibility: row.visibility,
            owner_login_name: row.owner_login_name.into(),
            posts_count: row.posts_count,
            is_known: row.is_known,
            has_participated: row.has_participated,
        })
        .collect())
}

#[cfg(test)]
mod move_destination_tests {
    use super::{may_move_post_into, move_destinations};
    use sqlx::{Postgres, Transaction};
    use uuid::Uuid;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    /// `do_post_edit_community` takes the destination from the form, so this
    /// is all that stands between a hand-made POST and a community the form
    /// would never have offered. The author here is a member of the private
    /// one, which is still not enough.
    #[tokio::test]
    async fn only_a_public_or_unlisted_community_takes_a_moved_post() {
        let Some(mut tx) = tx().await else { return };
        let tag = Uuid::new_v4().simple().to_string()[..12].to_string();
        let author = sqlx::query_scalar!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
            format!("mover_{tag}")
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();

        let mut ids = std::collections::HashMap::new();
        for kind in ["public", "unlisted", "private", "twotone", "deleted"] {
            let slug = format!("{kind}_{tag}");
            let (visibility, colours) = match kind {
                "twotone" => ("public", Some("#000000")),
                "deleted" => ("public", None),
                other => (other, None),
            };
            let id = sqlx::query_scalar!(
                r#"INSERT INTO communities
                   (owner_id, name, description, visibility, slug, foreground_color, background_color, deleted_at)
                   VALUES ($1, $2, '', $3::text::community_visibility, $2, $4, $4,
                           CASE WHEN $5 THEN now() END)
                   RETURNING id"#,
                author,
                slug,
                visibility,
                colours,
                kind == "deleted"
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            ids.insert(kind, id);
        }
        sqlx::query!(
            "INSERT INTO community_members (community_id, user_id, role) VALUES ($1, $2, 'owner')",
            ids["private"],
            author
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        for (kind, expected) in [
            ("public", true),
            ("unlisted", true),
            ("private", false),
            ("twotone", false),
            ("deleted", false),
        ] {
            assert_eq!(
                may_move_post_into(&mut tx, ids[kind]).await.unwrap(),
                expected,
                "{kind}"
            );
        }
        assert!(!may_move_post_into(&mut tx, Uuid::new_v4()).await.unwrap());
    }

    /// The move page's list: never private or two-tone, unlisted only for
    /// whoever owns or has posted in it, never the post's own community,
    /// the mover's own whole and the rest cut at the limit (one over, so
    /// the page can say there are more), and the search taken literally.
    #[tokio::test]
    async fn the_move_list_offers_what_a_move_would_take() {
        let Some(mut tx) = tx().await else { return };
        let tag = Uuid::new_v4().simple().to_string()[..12].to_string();
        let user = |name: String| {
            sqlx::query_scalar!(
                "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
                name
            )
        };
        let mover = user(format!("mv_{tag}")).fetch_one(&mut *tx).await.unwrap();
        let other = user(format!("ot_{tag}")).fetch_one(&mut *tx).await.unwrap();

        let mut ids = std::collections::HashMap::new();
        for (kind, owner, visibility, two_tone) in [
            ("mine_unlisted", mover, "unlisted", false),
            ("their_unlisted", other, "unlisted", false),
            ("private", mover, "private", false),
            ("twotone", other, "public", true),
            ("current", other, "public", false),
            ("pub_a", other, "public", false),
            ("pub_b", other, "public", false),
            ("pub_c", other, "public", false),
            ("pct_d", other, "public", false),
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
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            // Something published, so the public ones are offered at all.
            let image = sqlx::query_scalar!(
                "INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
                 VALUES (1, 1, '1 second', 1, $1, 'neo') RETURNING id",
                format!("{kind}_{tag}.png")
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            sqlx::query!(
                "INSERT INTO posts (author_id, community_id, image_id, published_at) VALUES ($1, $2, $3, now())",
                other,
                id,
                image
            )
            .execute(&mut *tx)
            .await
            .unwrap();
            ids.insert(kind, id);
        }

        let names = |rows: &[super::MoveDestination]| {
            rows.iter()
                .map(|c| c.name.trim_end_matches(&format!("_{tag}")).to_string())
                .collect::<Vec<_>>()
        };

        let all = move_destinations(&mut tx, mover, Some(ids["current"]), &tag, 10)
            .await
            .unwrap();
        let mut offered = names(&all);
        offered.sort();
        assert_eq!(
            offered,
            ["mine_unlisted", "pct_d", "pub_a", "pub_b", "pub_c"]
        );
        assert!(all[0].is_known, "the mover's own come first");

        // Two of the four others, plus one to say there are more; the
        // mover's own are not counted against the limit.
        let cut = move_destinations(&mut tx, mover, Some(ids["current"]), &tag, 2)
            .await
            .unwrap();
        assert_eq!(cut.iter().filter(|c| !c.is_known).count(), 3);
        assert_eq!(cut.iter().filter(|c| c.is_known).count(), 1);

        // `pub_` is a prefix of three names; `_` and `%` are characters, not
        // wildcards, so `pct%` finds nothing and `pub_` not `pct_d`.
        let pub_ = move_destinations(&mut tx, mover, None, "pub_", 10)
            .await
            .unwrap();
        assert!(
            names(&pub_).iter().all(|n| n.starts_with("pub_")),
            "{:?}",
            names(&pub_)
        );
        let pct = move_destinations(&mut tx, mover, None, "pct%", 10)
            .await
            .unwrap();
        assert!(pct.is_empty());
    }
}
