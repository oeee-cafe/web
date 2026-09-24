//! Post queries with **no visibility filtering at all**.
//!
//! Every query here deliberately omits the `communities.visibility = 'public'`
//! / `community_members` predicates and the `deleted_at IS NULL` guard that the
//! rest of `models` applies. They exist so admins can review private-community
//! posts, unpublished drafts and soft-deleted content.
//!
//! Keeping them in one module is the security boundary: nothing here may be
//! called from a handler that does not take the `AdminUser` extractor. Do not
//! add a `viewer_id` parameter to these — if a caller needs visibility rules,
//! it belongs in `models::post` instead.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{query, query_as, Postgres, Transaction};
use uuid::Uuid;

use crate::models::community::CommunityVisibility;
use crate::models::post::PostDeletionReason;
use crate::models::user::UserRole;

/// A post row as the admin views render it: author and community are joined in
/// so the global list needs no N+1 lookups.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminPost {
    pub id: Uuid,
    pub title: Option<String>,
    pub is_sensitive: bool,
    pub viewer_count: i32,
    pub author_id: Uuid,
    pub author_login_name: String,
    pub author_display_name: String,
    pub community_id: Option<Uuid>,
    pub community_slug: Option<String>,
    pub community_name: Option<String>,
    pub community_visibility: Option<CommunityVisibility>,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub deletion_reason: Option<PostDeletionReason>,
    /// Author's own sensitive tick. Kept separate from `is_explicit` here — the
    /// public queries collapse the two, but staff need to see which is which.
    pub is_sensitive_by_author: bool,
    /// Staff flag. Survives author edits.
    pub is_explicit: bool,
    pub explicit_flagged_at: Option<DateTime<Utc>>,
    pub explicit_flagged_by_login_name: Option<String>,
}

impl AdminPost {
    /// Drafts have never been published, so nobody but the author has seen them.
    pub fn is_draft(&self) -> bool {
        self.published_at.is_none()
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// Which slice of the firehose to show. `None` on the id fields means "any";
/// the booleans default to excluding drafts and deleted posts so the global
/// list opens on live content.
#[derive(Clone, Copy, Debug, Default)]
pub struct AdminPostFilter {
    pub author_id: Option<Uuid>,
    pub community_id: Option<Uuid>,
    pub include_drafts: bool,
    pub include_deleted: bool,
}

/// Every post on the instance, newest first, regardless of community
/// visibility, membership, publication state or soft deletion.
///
/// Ordered by `COALESCE(published_at, created_at)` so drafts interleave by the
/// time they were drawn rather than sinking to the bottom on a NULL sort key.
pub async fn find_all_posts(
    tx: &mut Transaction<'_, Postgres>,
    filter: AdminPostFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<AdminPost>> {
    let rows = query_as!(
        AdminPost,
        r#"
        SELECT
            posts.id,
            posts.title,
            posts.is_sensitive,
            posts.viewer_count,
            posts.author_id,
            users.login_name AS author_login_name,
            users.display_name AS author_display_name,
            posts.community_id,
            communities.slug AS "community_slug?",
            communities.name AS "community_name?",
            communities.visibility AS "community_visibility?: CommunityVisibility",
            images.image_filename,
            images.width AS image_width,
            images.height AS image_height,
            posts.published_at,
            posts.created_at,
            posts.deleted_at,
            posts.deletion_reason AS "deletion_reason?: PostDeletionReason",
            posts.is_sensitive AS is_sensitive_by_author,
            posts.is_explicit,
            posts.explicit_flagged_at,
            flagger.login_name AS "explicit_flagged_by_login_name?"
        FROM posts
        JOIN users ON posts.author_id = users.id
        JOIN images ON posts.image_id = images.id
        LEFT JOIN communities ON posts.community_id = communities.id
        LEFT JOIN users flagger ON posts.explicit_flagged_by = flagger.id
        WHERE ($1::uuid IS NULL OR posts.author_id = $1)
          AND ($2::uuid IS NULL OR posts.community_id = $2)
          AND ($3 OR posts.published_at IS NOT NULL)
          AND ($4 OR posts.deleted_at IS NULL)
        ORDER BY COALESCE(posts.published_at, posts.created_at) DESC
        LIMIT $5 OFFSET $6
        "#,
        filter.author_id,
        filter.community_id,
        filter.include_drafts,
        filter.include_deleted,
        limit,
        offset,
    );
    Ok(rows.fetch_all(&mut **tx).await?)
}

/// Total matching `filter`, for pagination. Mirrors `find_all_posts`' WHERE
/// clause exactly — change both together.
pub async fn count_all_posts(
    tx: &mut Transaction<'_, Postgres>,
    filter: AdminPostFilter,
) -> Result<i64> {
    let row = query!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM posts
        WHERE ($1::uuid IS NULL OR posts.author_id = $1)
          AND ($2::uuid IS NULL OR posts.community_id = $2)
          AND ($3 OR posts.published_at IS NOT NULL)
          AND ($4 OR posts.deleted_at IS NULL)
        "#,
        filter.author_id,
        filter.community_id,
        filter.include_drafts,
        filter.include_deleted,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.count)
}

/// A single post, including soft-deleted ones. `models::post::find_post_by_id`
/// hides those, which is exactly what moderation review needs to see.
pub async fn find_post_by_id(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Option<AdminPost>> {
    let row = query_as!(
        AdminPost,
        r#"
        SELECT
            posts.id,
            posts.title,
            posts.is_sensitive,
            posts.viewer_count,
            posts.author_id,
            users.login_name AS author_login_name,
            users.display_name AS author_display_name,
            posts.community_id,
            communities.slug AS "community_slug?",
            communities.name AS "community_name?",
            communities.visibility AS "community_visibility?: CommunityVisibility",
            images.image_filename,
            images.width AS image_width,
            images.height AS image_height,
            posts.published_at,
            posts.created_at,
            posts.deleted_at,
            posts.deletion_reason AS "deletion_reason?: PostDeletionReason",
            posts.is_sensitive AS is_sensitive_by_author,
            posts.is_explicit,
            posts.explicit_flagged_at,
            flagger.login_name AS "explicit_flagged_by_login_name?"
        FROM posts
        JOIN users ON posts.author_id = users.id
        JOIN images ON posts.image_id = images.id
        LEFT JOIN communities ON posts.community_id = communities.id
        LEFT JOIN users flagger ON posts.explicit_flagged_by = flagger.id
        WHERE posts.id = $1
        "#,
        post_id,
    );
    Ok(row.fetch_optional(&mut **tx).await?)
}

/// Sets or clears the staff explicit flag on a post. Takes the desired state
/// rather than toggling so a double-submit is idempotent.
///
/// Nothing in the author-facing edit path writes this column, which is what
/// makes the flag survive the author saving the post again.
pub async fn set_post_explicit(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    is_explicit: bool,
    flagged_by: Uuid,
) -> Result<()> {
    query!(
        r#"
        UPDATE posts
        SET is_explicit = $2,
            explicit_flagged_at = CASE WHEN $2 THEN now() ELSE NULL END,
            explicit_flagged_by = CASE WHEN $2 THEN $3::uuid ELSE NULL END
        WHERE id = $1
        "#,
        post_id,
        is_explicit,
        flagged_by,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Ordering for the user, community and collaborative-session lists.
///
/// Passed into SQL as a string and matched inside `CASE` expressions rather
/// than interpolated, so the ORDER BY stays compile-time checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminSort {
    /// Most recently active first. The default: on a moderation surface, who
    /// did something lately is the more useful question than who signed up.
    #[default]
    Active,
    Created,
    Name,
}

impl AdminSort {
    fn as_sql(self) -> &'static str {
        match self {
            AdminSort::Active => "active",
            AdminSort::Created => "created",
            AdminSort::Name => "name",
        }
    }
}

/// Community summary for the filter dropdown and the per-community header.
/// Includes private and unlisted communities.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminCommunity {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub visibility: CommunityVisibility,
    pub deleted_at: Option<DateTime<Utc>>,
}

pub async fn find_all_communities(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<AdminCommunity>> {
    let rows = query_as!(
        AdminCommunity,
        r#"
        SELECT
            id,
            slug,
            name,
            visibility AS "visibility: CommunityVisibility",
            deleted_at
        FROM communities
        ORDER BY name ASC
        "#,
    );
    Ok(rows.fetch_all(&mut **tx).await?)
}

pub async fn find_community_by_slug(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
) -> Result<Option<AdminCommunity>> {
    let row = query_as!(
        AdminCommunity,
        r#"
        SELECT
            id,
            slug,
            name,
            visibility AS "visibility: CommunityVisibility",
            deleted_at
        FROM communities
        WHERE slug = $1
        "#,
        slug,
    );
    Ok(row.fetch_optional(&mut **tx).await?)
}

/// Community list with activity, for the communities page. Kept separate from
/// `find_all_communities` because that one feeds the filter dropdown on every
/// `/admin/posts` render and does not need the correlated subqueries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminCommunityActivity {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub visibility: CommunityVisibility,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub post_count: i64,
    /// Most recent post in the community. `None` means nothing was ever posted.
    /// Live collaborative sessions are not counted.
    pub last_active_at: Option<DateTime<Utc>>,
}

pub async fn find_all_communities_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    sort: AdminSort,
) -> Result<Vec<AdminCommunityActivity>> {
    let rows = query_as!(
        AdminCommunityActivity,
        r#"
        SELECT
            id AS "id!",
            slug AS "slug!",
            name AS "name!",
            visibility AS "visibility!: CommunityVisibility",
            created_at AS "created_at!",
            deleted_at,
            post_count AS "post_count!",
            last_active_at
        FROM (
            SELECT
                c.id,
                c.slug,
                c.name,
                c.visibility,
                c.deleted_at,
                c.created_at,
                (
                    SELECT COUNT(*)
                    FROM posts p
                    WHERE p.community_id = c.id AND p.deleted_at IS NULL
                ) AS post_count,
                (
                    SELECT MAX(COALESCE(p.published_at, p.created_at))
                    FROM posts p
                    WHERE p.community_id = c.id AND p.deleted_at IS NULL
                ) AS last_active_at
            FROM communities c
        ) c
        ORDER BY
            CASE WHEN $1 = 'created' THEN c.created_at END DESC,
            CASE WHEN $1 = 'name' THEN c.name END ASC,
            c.last_active_at DESC NULLS LAST
        "#,
        sort.as_sql(),
    );
    Ok(rows.fetch_all(&mut **tx).await?)
}

/// Which slice of the session list to show.
///
/// A live session is one nobody has ended: the lobby's own listing turns on
/// exactly the same `ended_at IS NULL`, so "live" here means the same thing it
/// means to a person looking at the lobby.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminSessionStatus {
    #[default]
    All,
    Live,
    Ended,
}

impl AdminSessionStatus {
    fn as_sql(self) -> &'static str {
        match self {
            AdminSessionStatus::All => "all",
            AdminSessionStatus::Live => "live",
            AdminSessionStatus::Ended => "ended",
        }
    }
}

/// A collaborative session as the admin list renders it.
///
/// Link-only and private-community sessions included -- the lobby shows a
/// person their own sessions and the public ones, which leaves nowhere to see
/// a room that is filling up out of sight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminCollaborativeSession {
    pub id: Uuid,
    pub title: Option<String>,
    pub owner_login_name: String,
    pub width: i32,
    pub height: i32,
    pub max_participants: i32,
    /// Seats currently held. This is the number the lobby shows.
    pub active_participant_count: i64,
    /// Everyone who has ever joined, which is the one that says whether a quiet
    /// room was quiet all along or emptied out.
    pub total_participant_count: i64,
    /// False means link-only: it never appears in the public lobby.
    pub is_public: bool,
    pub community_slug: Option<String>,
    pub community_name: Option<String>,
    pub community_visibility: Option<CommunityVisibility>,
    pub created_at: chrono::NaiveDateTime,
    pub last_activity: chrono::NaiveDateTime,
    pub ended_at: Option<DateTime<Utc>>,
    /// Set once the owner saved the canvas as a post.
    pub saved_post_id: Option<Uuid>,
    /// Filled in from Redis after the query, the way the lobby fills it: the
    /// previews do not live in this table. See
    /// `web::handlers::collaborate::preview`.
    pub preview_version: Option<u64>,
}

/// Every session, in any state, with no visibility rule applied.
pub async fn find_all_collaborative_sessions(
    tx: &mut Transaction<'_, Postgres>,
    sort: AdminSort,
    status: AdminSessionStatus,
    limit: i64,
    offset: i64,
) -> Result<Vec<AdminCollaborativeSession>> {
    find_collaborative_sessions(tx, sort, status, None, limit, offset).await
}

/// One session as the list shows it, for the page that inspects it.
pub async fn find_collaborative_session(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<AdminCollaborativeSession>> {
    Ok(find_collaborative_sessions(
        tx,
        AdminSort::Active,
        AdminSessionStatus::All,
        Some(id),
        1,
        0,
    )
    .await?
    .into_iter()
    .next())
}

/// One query for both, so a session reads the same on the list and on its
/// own page.
async fn find_collaborative_sessions(
    tx: &mut Transaction<'_, Postgres>,
    sort: AdminSort,
    status: AdminSessionStatus,
    only: Option<Uuid>,
    limit: i64,
    offset: i64,
) -> Result<Vec<AdminCollaborativeSession>> {
    let rows = query!(
        r#"
        SELECT
            cs.id,
            cs.title,
            u.login_name AS owner_login_name,
            cs.width,
            cs.height,
            cs.max_participants,
            cs.is_public,
            c.slug AS "community_slug?",
            c.name AS "community_name?",
            c.visibility AS "community_visibility?: CommunityVisibility",
            cs.created_at,
            cs.last_activity,
            cs.ended_at,
            cs.saved_post_id,
            COUNT(csp.user_id) FILTER (WHERE csp.is_active) AS "active_participant_count!",
            COUNT(csp.user_id) AS "total_participant_count!"
        FROM collaborative_sessions cs
        JOIN users u ON cs.owner_id = u.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        WHERE ($1 = 'all'
           OR ($1 = 'live' AND cs.ended_at IS NULL)
           OR ($1 = 'ended' AND cs.ended_at IS NOT NULL))
          AND ($5::uuid IS NULL OR cs.id = $5)
        GROUP BY cs.id, u.login_name, c.slug, c.name, c.visibility
        ORDER BY
            CASE WHEN $2 = 'created' THEN cs.created_at END DESC,
            CASE WHEN $2 = 'name' THEN cs.title END ASC NULLS LAST,
            cs.last_activity DESC
        LIMIT $3 OFFSET $4
        "#,
        status.as_sql(),
        sort.as_sql(),
        limit,
        offset,
        only,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AdminCollaborativeSession {
            id: row.id,
            title: row.title,
            owner_login_name: row.owner_login_name,
            width: row.width,
            height: row.height,
            max_participants: row.max_participants,
            active_participant_count: row.active_participant_count,
            total_participant_count: row.total_participant_count,
            is_public: row.is_public,
            community_slug: row.community_slug,
            community_name: row.community_name,
            community_visibility: row.community_visibility,
            created_at: row.created_at,
            last_activity: row.last_activity,
            ended_at: row.ended_at,
            saved_post_id: row.saved_post_id,
            preview_version: None,
        })
        .collect())
}

/// Somebody who has been in a session, as its inspector lists them.
#[derive(Clone, Debug, Serialize)]
pub struct AdminSessionParticipant {
    pub user_id: Uuid,
    pub login_name: String,
    pub display_name: String,
    pub joined_at: chrono::NaiveDateTime,
    pub left_at: Option<chrono::NaiveDateTime>,
    pub is_active: bool,
}

/// Everyone who has ever joined a session, in the order they first did.
pub async fn find_collaborative_session_participants(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<Vec<AdminSessionParticipant>> {
    Ok(query_as!(
        AdminSessionParticipant,
        r#"
        SELECT
            csp.user_id,
            u.login_name,
            u.display_name,
            csp.joined_at,
            csp.left_at,
            csp.is_active
        FROM collaborative_sessions_participants csp
        JOIN users u ON csp.user_id = u.id
        WHERE csp.session_id = $1
        ORDER BY csp.joined_at
        "#,
        session_id,
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// A banner as the review queue shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminBanner {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_login_name: String,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub is_explicit: bool,
    pub flagged_at: Option<DateTime<Utc>>,
    pub flagged_by_login_name: Option<String>,
    /// True when this is the banner the author currently displays, which is the
    /// one /about would surface.
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// Banners for review, newest first. Excludes deleted ones — there is nothing
/// to moderate about a banner nobody can see.
pub async fn find_all_banners(
    tx: &mut Transaction<'_, Postgres>,
    only_explicit: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<AdminBanner>> {
    let rows = query_as!(
        AdminBanner,
        r#"
        SELECT
            b.id,
            b.author_id,
            author.login_name AS author_login_name,
            i.image_filename,
            i.width AS image_width,
            i.height AS image_height,
            b.is_explicit,
            b.flagged_at,
            flagger.login_name AS "flagged_by_login_name?",
            (author.banner_id = b.id) AS "is_active!",
            b.created_at
        FROM banners b
        JOIN users author ON b.author_id = author.id
        JOIN images i ON b.image_id = i.id
        LEFT JOIN users flagger ON b.flagged_by = flagger.id
        WHERE b.deleted_at IS NULL
          AND ($1 = false OR b.is_explicit)
        ORDER BY b.created_at DESC
        LIMIT $2 OFFSET $3
        "#,
        only_explicit,
        limit,
        offset,
    );
    Ok(rows.fetch_all(&mut **tx).await?)
}

/// Single banner, for re-rendering a card after a flag change.
pub async fn find_banner_by_id(
    tx: &mut Transaction<'_, Postgres>,
    banner_id: Uuid,
) -> Result<Option<AdminBanner>> {
    let row = query_as!(
        AdminBanner,
        r#"
        SELECT
            b.id,
            b.author_id,
            author.login_name AS author_login_name,
            i.image_filename,
            i.width AS image_width,
            i.height AS image_height,
            b.is_explicit,
            b.flagged_at,
            flagger.login_name AS "flagged_by_login_name?",
            (author.banner_id = b.id) AS "is_active!",
            b.created_at
        FROM banners b
        JOIN users author ON b.author_id = author.id
        JOIN images i ON b.image_id = i.id
        LEFT JOIN users flagger ON b.flagged_by = flagger.id
        WHERE b.id = $1
        "#,
        banner_id,
    );
    Ok(row.fetch_optional(&mut **tx).await?)
}

/// Sets or clears the explicit flag. Takes the desired state rather than
/// toggling so a double-submit cannot flip it back.
pub async fn set_banner_explicit(
    tx: &mut Transaction<'_, Postgres>,
    banner_id: Uuid,
    is_explicit: bool,
    flagged_by: Uuid,
) -> Result<()> {
    query!(
        r#"
        UPDATE banners
        SET is_explicit = $2,
            flagged_at = CASE WHEN $2 THEN now() ELSE NULL END,
            flagged_by = CASE WHEN $2 THEN $3::uuid ELSE NULL END
        WHERE id = $1
        "#,
        banner_id,
        is_explicit,
        flagged_by,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// User summary for the admin user list, including deleted accounts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminUserSummary {
    pub id: Uuid,
    pub login_name: String,
    pub display_name: String,
    pub role: UserRole,
    pub post_count: i64,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Latest content the account produced: a post, a comment or a guestbook
    /// entry. Reactions and passive browsing are not counted, so this reads as
    /// "last contributed" rather than "last signed in" — there is no session
    /// timestamp on `users` to derive the latter from.
    pub last_active_at: Option<DateTime<Utc>>,
}

pub async fn find_all_users(
    tx: &mut Transaction<'_, Postgres>,
    sort: AdminSort,
    limit: i64,
    offset: i64,
) -> Result<Vec<AdminUserSummary>> {
    let rows = query_as!(
        AdminUserSummary,
        r#"
        SELECT
            id AS "id!",
            login_name AS "login_name!",
            display_name AS "display_name!",
            role AS "role!: UserRole",
            created_at AS "created_at!",
            deleted_at,
            post_count AS "post_count!",
            last_active_at
        FROM (
            SELECT
                u.id,
                u.login_name,
                u.display_name,
                u.role,
                u.created_at,
                u.deleted_at,
                (
                    SELECT COUNT(*)
                    FROM posts p
                    WHERE p.author_id = u.id AND p.deleted_at IS NULL
                ) AS post_count,
                -- GREATEST ignores NULLs, so an account that has only ever
                -- commented still gets a timestamp.
                GREATEST(
                    (
                        SELECT MAX(COALESCE(p.published_at, p.created_at))
                        FROM posts p
                        WHERE p.author_id = u.id AND p.deleted_at IS NULL
                    ),
                    (
                        SELECT MAX(cm.created_at)
                        FROM comments cm
                        JOIN actors a ON cm.actor_id = a.id
                        WHERE a.user_id = u.id AND cm.deleted_at IS NULL
                    ),
                    (
                        SELECT MAX(g.created_at)
                        FROM guestbook_entries g
                        WHERE g.author_id = u.id
                    )
                ) AS last_active_at
            FROM users u
        ) u
        ORDER BY
            CASE WHEN $1 = 'created' THEN u.created_at END DESC,
            CASE WHEN $1 = 'name' THEN u.login_name END ASC,
            u.last_active_at DESC NULLS LAST
        LIMIT $2 OFFSET $3
        "#,
        sort.as_sql(),
        limit,
        offset,
    );
    Ok(rows.fetch_all(&mut **tx).await?)
}

