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
use crate::models::identity::Provider;
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
    /// The recording, `<sha256>.pch` (NEO) or `.tgkr` (Tegaki). None for a
    /// drawing saved without one.
    pub replay_filename: Option<String>,
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
            images.replay_filename,
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
            images.replay_filename,
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
    /// Needs a look: a client filed a report, or the replay came out
    /// different from what was saved.
    Flagged,
}

impl AdminSessionStatus {
    fn as_sql(self) -> &'static str {
        match self {
            AdminSessionStatus::All => "all",
            AdminSessionStatus::Live => "live",
            AdminSessionStatus::Ended => "ended",
            AdminSessionStatus::Flagged => "flagged",
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
    /// A guest opened it; there is no profile to link.
    pub owner_is_guest: bool,
    pub width: i32,
    pub height: i32,
    pub max_participants: i32,
    /// Seats currently held. This is the number the lobby shows.
    pub active_participant_count: i64,
    /// Everyone who has ever joined, which is the one that says whether a quiet
    /// room was quiet all along or emptied out.
    pub total_participant_count: i64,
    /// Watching now, without a seat. Not in the seat counts above.
    pub active_spectator_count: i64,
    /// Everybody who has only ever watched.
    pub total_spectator_count: i64,
    /// False means link-only: it never appears in the public lobby.
    pub is_public: bool,
    pub community_slug: Option<String>,
    pub community_name: Option<String>,
    pub community_visibility: Option<CommunityVisibility>,
    pub created_at: chrono::NaiveDateTime,
    pub last_activity: chrono::NaiveDateTime,
    pub ended_at: Option<DateTime<Utc>>,
    /// Set once the owner saved the canvas as a post.
    /// Put away after a while without drawing (`collaborate::pause`).
    pub paused_at: Option<DateTime<Utc>>,
    /// When a paused session ends if nobody comes back to it.
    pub paused_until: Option<DateTime<Utc>>,
    pub saved_post_id: Option<Uuid>,
    /// Filled in from Redis after the query, the way the lobby fills it: the
    /// previews do not live in this table. See
    /// `web::handlers::collaborate::preview`.
    pub preview_version: Option<u64>,
    /// What is known of its recording, or None when nothing has been noted --
    /// nothing recorded, or recorded before recordings were noted and never
    /// opened in the inspector since.
    pub recording: Option<AdminSessionRecording>,
}

/// A row of `collaborative_session_recordings`, as the list shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminSessionRecording {
    pub first_seq: Option<i64>,
    pub last_seq: Option<i64>,
    pub messages: i64,
    pub sealed: bool,
    pub chat_lines: i32,
    pub reports: i32,
    pub check_outcome: Option<String>,
    pub check_differing_pixels: Option<i64>,
    pub check_total_pixels: Option<i64>,
    pub check_note: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
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
            COALESCE(u.login_name, g.nickname) AS "owner_login_name!",
            cs.owner_guest_id IS NOT NULL AS "owner_is_guest!",
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
            cs.paused_at,
            cs.saved_post_id,
            COUNT(csp.participant_id) FILTER (WHERE csp.is_active AND NOT csp.watching) AS "active_participant_count!",
            COUNT(csp.participant_id) FILTER (WHERE NOT csp.is_spectator) AS "total_participant_count!",
            COUNT(csp.participant_id) FILTER (WHERE csp.is_active AND csp.watching) AS "active_spectator_count!",
            COUNT(csp.participant_id) FILTER (WHERE csp.is_spectator) AS "total_spectator_count!",
            r.session_id AS "recorded?",
            r.first_seq AS "first_seq?",
            r.last_seq AS "last_seq?",
            r.messages AS "messages?",
            r.sealed AS "sealed?",
            r.chat_lines AS "chat_lines?",
            r.reports AS "reports?",
            r.check_outcome AS "check_outcome?",
            r.check_differing_pixels AS "check_differing_pixels?",
            r.check_total_pixels AS "check_total_pixels?",
            r.check_note AS "check_note?",
            r.checked_at AS "checked_at?"
        FROM collaborative_sessions cs
        LEFT JOIN users u ON cs.owner_id = u.id
        LEFT JOIN collaborative_guests g ON cs.owner_guest_id = g.id
        LEFT JOIN communities c ON cs.community_id = c.id
        LEFT JOIN collaborative_sessions_participants csp ON cs.id = csp.session_id
        LEFT JOIN collaborative_session_recordings r ON r.session_id = cs.id
        WHERE ($1 = 'all'
           OR ($1 = 'live' AND cs.ended_at IS NULL)
           OR ($1 = 'ended' AND cs.ended_at IS NOT NULL)
           OR ($1 = 'flagged' AND (r.reports > 0 OR r.check_outcome = 'differs')))
          AND ($5::uuid IS NULL OR cs.id = $5)
        GROUP BY cs.id, u.login_name, g.nickname, c.slug, c.name, c.visibility, r.session_id
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
            owner_is_guest: row.owner_is_guest,
            width: row.width,
            height: row.height,
            max_participants: row.max_participants,
            active_participant_count: row.active_participant_count,
            total_participant_count: row.total_participant_count,
            active_spectator_count: row.active_spectator_count,
            total_spectator_count: row.total_spectator_count,
            is_public: row.is_public,
            community_slug: row.community_slug,
            community_name: row.community_name,
            community_visibility: row.community_visibility,
            created_at: row.created_at,
            last_activity: row.last_activity,
            ended_at: row.ended_at,
            paused_at: row.paused_at,
            paused_until: row
                .paused_at
                .map(crate::web::handlers::collaborate::pause::ends_at),
            saved_post_id: row.saved_post_id,
            preview_version: None,
            recording: row.recorded.map(|_| AdminSessionRecording {
                first_seq: row.first_seq,
                last_seq: row.last_seq,
                messages: row.messages.unwrap_or(0),
                sealed: row.sealed.unwrap_or(false),
                chat_lines: row.chat_lines.unwrap_or(0),
                reports: row.reports.unwrap_or(0),
                check_outcome: row.check_outcome,
                check_differing_pixels: row.check_differing_pixels,
                check_total_pixels: row.check_total_pixels,
                check_note: row.check_note,
                checked_at: row.checked_at,
            }),
        })
        .collect())
}

/// Somebody who has been in a session, as its inspector lists them.
#[derive(Clone, Debug, Serialize)]
pub struct AdminSessionParticipant {
    /// The participant id: an account's, or a guest's.
    pub user_id: Uuid,
    /// The account's login name, or the name a guest gave.
    pub login_name: String,
    pub display_name: String,
    /// No account and no profile, only the name.
    pub is_guest: bool,
    /// Only ever watched; drew nothing.
    pub is_spectator: bool,
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
            csp.participant_id AS user_id,
            COALESCE(u.login_name, g.nickname) AS "login_name!",
            COALESCE(u.display_name, g.nickname) AS "display_name!",
            csp.guest_id IS NOT NULL AS "is_guest!",
            csp.is_spectator,
            csp.joined_at,
            csp.left_at,
            csp.is_active
        FROM collaborative_sessions_participants csp
        LEFT JOIN users u ON csp.user_id = u.id
        LEFT JOIN collaborative_guests g ON csp.guest_id = g.id
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
            -- An author with no banner set compares to NULL, not to false.
            COALESCE(author.banner_id = b.id, false) AS "is_active!",
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
            -- An author with no banner set compares to NULL, not to false.
            COALESCE(author.banner_id = b.id, false) AS "is_active!",
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

/// A stretch of time to count in, as instants: from `start`, up to but not
/// including `end`. Either side left open runs to the beginning or the end
/// of time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdminRange {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

/// Which accounts a figure on the overview counted, so the user list can show
/// them. Each matches its figure in `find_overview` or
/// `find_sign_in_methods`; change them together.
///
/// For posts, comments, collaborations and purchases the figure counts the
/// things and the list the people, so the list can be the shorter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdminUserStat {
    Joined,
    Posted,
    /// Comments written here: a federated reply has no account to list.
    Commented,
    Collaborated,
    Purchased,
    ProviderSignup,
    ProviderLinked,
    ProviderUsed,
    ProviderNoPassword,
    PasswordSignup,
    PasswordSet,
}

impl AdminUserStat {
    /// As it is written in `?stat=`, and in the query.
    pub fn as_str(self) -> &'static str {
        match self {
            AdminUserStat::Joined => "joined",
            AdminUserStat::Posted => "posted",
            AdminUserStat::Commented => "commented",
            AdminUserStat::Collaborated => "collaborated",
            AdminUserStat::Purchased => "purchased",
            AdminUserStat::ProviderSignup => "provider-signup",
            AdminUserStat::ProviderLinked => "provider-linked",
            AdminUserStat::ProviderUsed => "provider-used",
            AdminUserStat::ProviderNoPassword => "provider-no-password",
            AdminUserStat::PasswordSignup => "password-signup",
            AdminUserStat::PasswordSet => "password-set",
        }
    }

    /// The ones that ask about a provider, and mean nothing without one.
    pub fn needs_provider(self) -> bool {
        matches!(
            self,
            AdminUserStat::ProviderSignup
                | AdminUserStat::ProviderLinked
                | AdminUserStat::ProviderUsed
                | AdminUserStat::ProviderNoPassword
        )
    }

    /// The ones that are a standing state rather than something done at a
    /// time, which a date range does not narrow.
    pub fn is_timeless(self) -> bool {
        matches!(
            self,
            AdminUserStat::ProviderLinked
                | AdminUserStat::ProviderNoPassword
                | AdminUserStat::PasswordSet
        )
    }

    /// What the list is of, for its heading.
    pub fn describe(self, provider: Option<Provider>) -> String {
        let provider = provider.map_or("a provider", Provider::display_name);
        match self {
            AdminUserStat::Joined => "Accounts made".to_string(),
            AdminUserStat::Posted => "Accounts that published a post".to_string(),
            AdminUserStat::Commented => "Accounts that commented".to_string(),
            AdminUserStat::Collaborated => "Accounts that started a collaboration".to_string(),
            AdminUserStat::Purchased => "Accounts that bought a Supporter Pack".to_string(),
            AdminUserStat::ProviderSignup => format!("Accounts made by signing in with {provider}"),
            AdminUserStat::ProviderLinked => format!("Accounts linked to {provider}"),
            AdminUserStat::ProviderUsed => format!("Accounts last signed in with {provider}"),
            AdminUserStat::ProviderNoPassword => {
                format!("Accounts linked to {provider} with no password")
            }
            AdminUserStat::PasswordSignup => "Accounts made with a password".to_string(),
            AdminUserStat::PasswordSet => "Accounts with a password".to_string(),
        }
    }
}

/// Narrows the user list to the accounts one overview figure counted.
#[derive(Clone, Copy, Debug)]
pub struct AdminUserFilter {
    pub stat: AdminUserStat,
    pub provider: Option<Provider>,
    pub range: AdminRange,
}

/// One page of accounts, and how many match in all.
pub async fn find_all_users(
    tx: &mut Transaction<'_, Postgres>,
    sort: AdminSort,
    filter: Option<AdminUserFilter>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<AdminUserSummary>, i64)> {
    let rows = query!(
        r#"
        SELECT
            id AS "id!",
            login_name AS "login_name!",
            display_name AS "display_name!",
            role AS "role!: UserRole",
            created_at AS "created_at!",
            deleted_at,
            post_count AS "post_count!",
            last_active_at,
            COUNT(*) OVER () AS "matching!"
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
            CROSS JOIN (
                SELECT
                    COALESCE($6::timestamptz, '-infinity') AS start,
                    COALESCE($7::timestamptz, 'infinity') AS finish
            ) r
            WHERE $4::text IS NULL OR (u.deleted_at IS NULL AND (
                ($4 = 'joined'
                    AND u.created_at >= r.start AND u.created_at < r.finish)
                OR ($4 = 'posted' AND EXISTS (
                    SELECT 1 FROM posts p
                    WHERE p.author_id = u.id AND p.deleted_at IS NULL
                      AND p.published_at >= r.start AND p.published_at < r.finish))
                OR ($4 = 'commented' AND EXISTS (
                    SELECT 1 FROM comments cm
                    JOIN actors a ON cm.actor_id = a.id
                    WHERE a.user_id = u.id AND cm.deleted_at IS NULL
                      AND cm.created_at >= r.start AND cm.created_at < r.finish))
                -- Zoneless, written in the connection's zone, which the cast
                -- reads it back in.
                OR ($4 = 'collaborated' AND EXISTS (
                    SELECT 1 FROM collaborative_sessions cs
                    WHERE cs.owner_id = u.id
                      AND cs.created_at::timestamptz >= r.start
                      AND cs.created_at::timestamptz < r.finish))
                OR ($4 = 'purchased' AND EXISTS (
                    SELECT 1 FROM supporter_purchases sp
                    WHERE sp.user_id = u.id AND sp.revoked_at IS NULL
                      AND sp.purchased_at >= r.start AND sp.purchased_at < r.finish))
                OR ($4 = 'provider-signup'
                    AND u.created_at >= r.start AND u.created_at < r.finish
                    AND EXISTS (
                        SELECT 1 FROM user_identities i
                        WHERE i.user_id = u.id AND i.provider = $5
                          AND i.created_at = u.created_at))
                OR ($4 = 'provider-linked' AND EXISTS (
                    SELECT 1 FROM user_identities i
                    WHERE i.user_id = u.id AND i.provider = $5))
                OR ($4 = 'provider-used' AND EXISTS (
                    SELECT 1 FROM user_identities i
                    WHERE i.user_id = u.id AND i.provider = $5
                      AND i.last_used_at >= r.start AND i.last_used_at < r.finish))
                OR ($4 = 'provider-no-password' AND u.password_hash IS NULL
                    AND EXISTS (
                        SELECT 1 FROM user_identities i
                        WHERE i.user_id = u.id AND i.provider = $5))
                OR ($4 = 'password-signup'
                    AND u.created_at >= r.start AND u.created_at < r.finish
                    AND NOT EXISTS (
                        SELECT 1 FROM user_identities i
                        WHERE i.user_id = u.id AND i.created_at = u.created_at))
                OR ($4 = 'password-set' AND u.password_hash IS NOT NULL)
            ))
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
        filter.map(|filter| filter.stat.as_str()),
        filter
            .and_then(|filter| filter.provider)
            .map(Provider::as_str),
        filter.and_then(|filter| filter.range.start),
        filter.and_then(|filter| filter.range.end),
    )
    .fetch_all(&mut **tx)
    .await?;

    let matching = rows.first().map_or(0, |row| row.matching);
    let users = rows
        .into_iter()
        .map(|row| AdminUserSummary {
            id: row.id,
            login_name: row.login_name,
            display_name: row.display_name,
            role: row.role,
            post_count: row.post_count,
            created_at: row.created_at,
            deleted_at: row.deleted_at,
            last_active_at: row.last_active_at,
        })
        .collect();
    Ok((users, matching))
}

/// How many of one thing happened in the range, and ever.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct AdminActivity {
    pub in_range: i64,
    pub total: i64,
}

/// The figures on the admin overview, all read in one round trip.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AdminOverview {
    /// Accounts created, deleted ones left out of every figure.
    pub signups: AdminActivity,
    /// Posts by when they were published; drafts and removed posts excluded.
    pub posts: AdminActivity,
    /// Comments, federated replies included, removed ones excluded.
    pub comments: AdminActivity,
    pub collaborations: AdminActivity,
    /// Supporter purchases by when they were made, revoked ones excluded.
    pub purchases: AdminActivity,
    /// Sessions nobody has ended, the same `ended_at IS NULL` the lobby lists.
    pub live_sessions: i64,
    /// The collaborations list's "Needs a look": a report was filed or the
    /// replay check came out different. Counts every such session, looked at
    /// or not -- there is no "reviewed" mark to subtract.
    pub flagged_sessions: i64,
    /// Banners put up this week, which is how far back the queue is worth
    /// paging after a quiet week.
    pub new_banners: i64,
}

/// The overview's figures for `range`. Each in-range figure has a matching
/// `AdminUserStat` in `find_all_users`; change them together.
pub async fn find_overview(
    tx: &mut Transaction<'_, Postgres>,
    range: AdminRange,
) -> Result<AdminOverview> {
    let row = query!(
        r#"
        WITH
            r AS (
                SELECT
                    COALESCE($1::timestamptz, '-infinity') AS start,
                    COALESCE($2::timestamptz, 'infinity') AS finish
            ),
            u AS (
                SELECT
                    COUNT(*) FILTER (WHERE created_at >= r.start AND created_at < r.finish) AS in_range,
                    COUNT(*) AS total
                FROM users, r
                WHERE deleted_at IS NULL
            ),
            p AS (
                SELECT
                    COUNT(*) FILTER (WHERE published_at >= r.start AND published_at < r.finish) AS in_range,
                    COUNT(*) AS total
                FROM posts, r
                WHERE published_at IS NOT NULL AND deleted_at IS NULL
            ),
            c AS (
                SELECT
                    COUNT(*) FILTER (WHERE created_at >= r.start AND created_at < r.finish) AS in_range,
                    COUNT(*) AS total
                FROM comments, r
                WHERE deleted_at IS NULL
            ),
            -- created_at here is a timestamp without a zone, filled by its
            -- default now() in the connection's zone, which the cast reads
            -- it back in.
            s AS (
                SELECT
                    COUNT(*) FILTER (WHERE created_at::timestamptz >= r.start
                        AND created_at::timestamptz < r.finish) AS in_range,
                    COUNT(*) AS total,
                    COUNT(*) FILTER (WHERE ended_at IS NULL) AS live
                FROM collaborative_sessions, r
            ),
            f AS (
                SELECT COUNT(*) AS flagged
                FROM collaborative_session_recordings
                WHERE reports > 0 OR check_outcome = 'differs'
            ),
            sp AS (
                SELECT
                    COUNT(*) FILTER (WHERE purchased_at >= r.start AND purchased_at < r.finish) AS in_range,
                    COUNT(*) AS total
                FROM supporter_purchases, r
                WHERE revoked_at IS NULL
            ),
            b AS (
                SELECT COUNT(*) AS new
                FROM banners
                WHERE created_at > now() - interval '7 days' AND deleted_at IS NULL
            )
        SELECT
            u.in_range AS "signups_in_range!", u.total AS "signups_total!",
            p.in_range AS "posts_in_range!", p.total AS "posts_total!",
            c.in_range AS "comments_in_range!", c.total AS "comments_total!",
            s.in_range AS "sessions_in_range!", s.total AS "sessions_total!",
            s.live AS "live_sessions!",
            f.flagged AS "flagged_sessions!",
            sp.in_range AS "purchases_in_range!", sp.total AS "purchases_total!",
            b.new AS "new_banners!"
        FROM u, p, c, s, f, sp, b
        "#,
        range.start,
        range.end,
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(AdminOverview {
        signups: AdminActivity {
            in_range: row.signups_in_range,
            total: row.signups_total,
        },
        posts: AdminActivity {
            in_range: row.posts_in_range,
            total: row.posts_total,
        },
        comments: AdminActivity {
            in_range: row.comments_in_range,
            total: row.comments_total,
        },
        collaborations: AdminActivity {
            in_range: row.sessions_in_range,
            total: row.sessions_total,
        },
        purchases: AdminActivity {
            in_range: row.purchases_in_range,
            total: row.purchases_total,
        },
        live_sessions: row.live_sessions,
        flagged_sessions: row.flagged_sessions,
        new_banners: row.new_banners,
    })
}

/// One way into an account, as the overview's sign-in table shows it: a
/// provider, or a password.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminSignInMethod {
    pub name: String,
    /// `None` for a password.
    pub provider: Option<Provider>,
    /// Accounts made by signing in this way, in the range and ever.
    pub signups: AdminActivity,
    /// Live accounts that can sign in this way: linked to the provider, or
    /// with a password set.
    pub accounts: i64,
    /// Accounts whose last sign-in with the provider falls in the range. A
    /// link counts as a sign-in, and only the last is kept, so a range that
    /// ends before today misses anyone who has signed in again since. `None`
    /// for a password, whose sign-ins are not kept.
    pub used: Option<i64>,
    /// Linked accounts with no password, for which a provider (this or
    /// another) or a reset by email is the only way back in.
    pub without_password: Option<i64>,
}

/// The sign-in table: each provider in `Provider::ALL`'s order, then
/// passwords.
///
/// An account made through a provider is created and linked in one
/// transaction (`web::handlers::identity`), so its identity's `created_at`
/// is its own to the microsecond; that equality is what tells a sign-up
/// through a provider from a link added later. An account that made itself
/// with a provider and unlinked it since reads as a password sign-up.
pub async fn find_sign_in_methods(
    tx: &mut Transaction<'_, Postgres>,
    range: AdminRange,
) -> Result<Vec<AdminSignInMethod>> {
    let providers: Vec<String> = Provider::ALL
        .iter()
        .map(|provider| provider.as_str().to_string())
        .collect();
    let rows = query!(
        r#"
        SELECT
            p.provider AS "provider!",
            COUNT(u.id) FILTER (WHERE i.created_at = u.created_at
                AND u.created_at >= r.start AND u.created_at < r.finish) AS "signups_in_range!",
            COUNT(u.id) FILTER (WHERE i.created_at = u.created_at) AS "signups_total!",
            COUNT(u.id) AS "accounts!",
            COUNT(u.id) FILTER (WHERE i.last_used_at >= r.start
                AND i.last_used_at < r.finish) AS "used!",
            COUNT(u.id) FILTER (WHERE u.password_hash IS NULL) AS "without_password!"
        FROM unnest($1::text[]) WITH ORDINALITY AS p(provider, ord)
        CROSS JOIN (
            SELECT
                COALESCE($2::timestamptz, '-infinity') AS start,
                COALESCE($3::timestamptz, 'infinity') AS finish
        ) r
        LEFT JOIN user_identities i ON i.provider = p.provider
        LEFT JOIN users u ON u.id = i.user_id AND u.deleted_at IS NULL
        GROUP BY p.provider, p.ord
        ORDER BY p.ord
        "#,
        &providers,
        range.start,
        range.end,
    )
    .fetch_all(&mut **tx)
    .await?;

    let password = query!(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE NOT made_by_provider
                AND created_at >= COALESCE($1::timestamptz, '-infinity')
                AND created_at < COALESCE($2::timestamptz, 'infinity')) AS "signups_in_range!",
            COUNT(*) FILTER (WHERE NOT made_by_provider) AS "signups_total!",
            COUNT(*) FILTER (WHERE has_password) AS "accounts!"
        FROM (
            SELECT
                u.created_at,
                u.password_hash IS NOT NULL AS has_password,
                EXISTS (
                    SELECT 1 FROM user_identities i
                    WHERE i.user_id = u.id AND i.created_at = u.created_at
                ) AS made_by_provider
            FROM users u
            WHERE u.deleted_at IS NULL
        ) u
        "#,
        range.start,
        range.end,
    )
    .fetch_one(&mut **tx)
    .await?;

    let mut methods: Vec<AdminSignInMethod> = rows
        .into_iter()
        .filter_map(|row| {
            let provider = Provider::parse(&row.provider)?;
            Some(AdminSignInMethod {
                name: provider.display_name().to_string(),
                provider: Some(provider),
                signups: AdminActivity {
                    in_range: row.signups_in_range,
                    total: row.signups_total,
                },
                accounts: row.accounts,
                used: Some(row.used),
                without_password: Some(row.without_password),
            })
        })
        .collect();
    methods.push(AdminSignInMethod {
        name: "Password".to_string(),
        provider: None,
        signups: AdminActivity {
            in_range: password.signups_in_range,
            total: password.signups_total,
        },
        accounts: password.accounts,
        used: None,
        without_password: None,
    });
    Ok(methods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    /// A banner whose author has none set, as an account would after its
    /// active banner were cleared, is listed as not active rather than
    /// failing the whole queue.
    #[tokio::test]
    async fn a_banner_whose_author_has_none_set_is_not_active() {
        let Some(mut tx) = tx().await else { return };
        let author = query!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
            format!("b{}", &Uuid::new_v4().simple().to_string()[..12]),
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .id;
        let image = query!(
            r#"
            INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
            VALUES (200, 40, '0'::interval, 0, $1, 'neo')
            RETURNING id
            "#,
            format!("{}.png", Uuid::new_v4()),
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .id;
        let banner = query!(
            "INSERT INTO banners (author_id, image_id) VALUES ($1, $2) RETURNING id",
            author,
            image,
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .id;

        let listed = find_all_banners(&mut tx, false, 1000, 0).await.unwrap();
        let listed = listed.iter().find(|b| b.id == banner).expect("listed");
        assert!(!listed.is_active);
        let found = find_banner_by_id(&mut tx, banner).await.unwrap().unwrap();
        assert!(!found.is_active);

        query!(
            "UPDATE users SET banner_id = $1 WHERE id = $2",
            banner,
            author
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        let found = find_banner_by_id(&mut tx, banner).await.unwrap().unwrap();
        assert!(found.is_active);
    }
}
