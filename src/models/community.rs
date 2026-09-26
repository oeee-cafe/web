use crate::models::handle::LoginName;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::query;
use sqlx::query_as;
use sqlx::Postgres;
use sqlx::Transaction;
use sqlx::Type;
use uuid::Uuid;

use super::post::SerializablePost;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type, PartialEq, Eq)]
#[sqlx(type_name = "community_visibility", rename_all = "snake_case")]
#[serde(rename_all = "lowercase")]
pub enum CommunityVisibility {
    Public,
    Unlisted,
    Private,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type, PartialEq, Eq)]
#[sqlx(type_name = "community_member_role", rename_all = "snake_case")]
pub enum CommunityMemberRole {
    Owner,
    Moderator,
    Member,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommunityMember {
    pub id: Uuid,
    pub community_id: Uuid,
    pub user_id: Uuid,
    pub role: CommunityMemberRole,
    pub joined_at: DateTime<Utc>,
    pub invited_by: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Type, PartialEq, Eq)]
#[sqlx(type_name = "community_invitation_status", rename_all = "snake_case")]
pub enum CommunityInvitationStatus {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommunityInvitation {
    pub id: Uuid,
    pub community_id: Uuid,
    pub inviter_id: Uuid,
    pub invitee_id: Uuid,
    pub status: CommunityInvitationStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Community {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PublicCommunity {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub owner_login_name: LoginName,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub posts_count: Option<i64>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PublicCommunityWithPosts {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub owner_login_name: LoginName,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub posts: Vec<SerializablePost>,
}

impl Community {
    pub fn get_url(&self) -> String {
        format!("/communities/@{}", self.slug)
    }
}

pub struct CommunityDraft {
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct CommunityStats {
    pub total_posts: i64,
    pub total_contributors: i64,
    pub total_comments: i64,
}

pub async fn get_own_communities(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
) -> Result<Vec<Community>> {
    let q = query_as!(
        Community,
        r#"SELECT id, owner_id, name, slug, description, visibility as "visibility: _", updated_at, created_at, background_color, foreground_color FROM communities WHERE owner_id = $1 AND deleted_at IS NULL"#,
        owner_id
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn get_communities(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Community>> {
    let q = query_as!(
        Community,
        r#"SELECT id, owner_id, name, slug, description, visibility as "visibility: _", updated_at, created_at, background_color, foreground_color FROM communities WHERE deleted_at IS NULL"#
    );
    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn get_public_communities(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<PublicCommunity>> {
    // Select communities ordered by latest published post
    let q = query_as!(
        PublicCommunity,
        r#"
            SELECT communities.id, communities.owner_id, users.login_name AS owner_login_name, communities.name, communities.slug, communities.description, communities.visibility as "visibility: _", communities.updated_at, communities.created_at, communities.background_color, communities.foreground_color, COALESCE(COUNT(posts.id), 0) AS posts_count
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id AND posts.published_at IS NOT NULL AND posts.deleted_at IS NULL
            LEFT JOIN users ON communities.owner_id = users.id
            WHERE communities.visibility = 'public' AND communities.deleted_at IS NULL
            GROUP BY communities.id, users.login_name
            HAVING MAX(posts.published_at) IS NOT NULL
            ORDER BY MAX(posts.published_at) DESC
        "#
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

/// Ordering for the public community directory.
///
/// Passed into SQL as a string and matched inside `CASE` expressions rather
/// than interpolated, so the ORDER BY stays compile-time checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommunitySort {
    /// Most recently posted-in first. The default: a directory of 90-odd
    /// communities is only browsable if the live ones surface.
    #[default]
    Active,
    Posts,
    Newest,
    Name,
}

impl CommunitySort {
    /// Value used both as the SQL discriminator and the URL query parameter,
    /// so a sentinel URL round-trips back to the same ordering.
    pub fn as_param(self) -> &'static str {
        self.as_sql()
    }

    fn as_sql(self) -> &'static str {
        match self {
            CommunitySort::Active => "active",
            CommunitySort::Posts => "posts",
            CommunitySort::Newest => "newest",
            CommunitySort::Name => "name",
        }
    }
}

pub async fn get_public_communities_paginated(
    tx: &mut Transaction<'_, Postgres>,
    sort: CommunitySort,
    limit: i64,
    offset: i64,
) -> Result<Vec<PublicCommunity>> {
    // Every branch falls through to MAX(published_at) so the ordering is total
    // and pagination cannot repeat or skip a community across batches.
    let q = query_as!(
        PublicCommunity,
        r#"
            SELECT communities.id, communities.owner_id, users.login_name AS owner_login_name, communities.name, communities.slug, communities.description, communities.visibility as "visibility: _", communities.updated_at, communities.created_at, communities.background_color, communities.foreground_color, COALESCE(COUNT(posts.id), 0) AS posts_count
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id AND posts.published_at IS NOT NULL AND posts.deleted_at IS NULL
            LEFT JOIN users ON communities.owner_id = users.id
            WHERE communities.visibility = 'public' AND communities.deleted_at IS NULL
            GROUP BY communities.id, users.login_name
            HAVING MAX(posts.published_at) IS NOT NULL
            ORDER BY
                CASE WHEN $1 = 'posts' THEN COUNT(posts.id) END DESC,
                CASE WHEN $1 = 'newest' THEN communities.created_at END DESC,
                CASE WHEN $1 = 'name' THEN communities.name END ASC,
                MAX(posts.published_at) DESC
            LIMIT $2 OFFSET $3
        "#,
        sort.as_sql(),
        limit,
        offset
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn search_public_communities(
    tx: &mut Transaction<'_, Postgres>,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<PublicCommunity>> {
    // Prepare search pattern
    let pattern = format!("%{}%", query);
    let exact_pattern = query.to_string();

    // Search communities by name, slug, or description with ranking
    let q = query_as!(
        PublicCommunity,
        r#"
            SELECT communities.id, communities.owner_id, users.login_name AS owner_login_name,
                   communities.name, communities.slug, communities.description,
                   communities.visibility as "visibility: _", communities.updated_at, communities.created_at,
                   communities.background_color, communities.foreground_color,
                   COALESCE(COUNT(posts.id), 0) AS posts_count
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id AND posts.published_at IS NOT NULL AND posts.deleted_at IS NULL
            LEFT JOIN users ON communities.owner_id = users.id
            WHERE communities.visibility = 'public' AND communities.deleted_at IS NULL
              AND (communities.name ILIKE $1
                   OR communities.slug ILIKE $1
                   OR communities.description ILIKE $1)
            GROUP BY communities.id, users.login_name
            ORDER BY
                CASE
                    WHEN communities.name ILIKE $2 THEN 0  -- Exact match
                    WHEN communities.slug ILIKE $2 THEN 1  -- Slug match
                    WHEN communities.name ILIKE $3 THEN 2  -- Starts with match (name)
                    WHEN communities.slug ILIKE $3 THEN 3  -- Starts with match (slug)
                    ELSE 4
                END,
                communities.name
            LIMIT $4 OFFSET $5
        "#,
        pattern,
        exact_pattern,
        format!("{}%", query),
        limit,
        offset
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn get_active_public_communities_excluding_owner(
    tx: &mut Transaction<'_, Postgres>,
    community_owner_id: Uuid,
) -> Result<Vec<PublicCommunity>> {
    let q = query_as!(
        PublicCommunity,
        r#"
            SELECT communities.id, communities.owner_id, users.login_name AS owner_login_name, communities.name, communities.slug, communities.description, communities.visibility as "visibility: _", communities.updated_at, communities.created_at, communities.background_color, communities.foreground_color, COUNT(posts.id) AS posts_count
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id AND posts.published_at IS NOT NULL AND posts.deleted_at IS NULL
            LEFT JOIN users ON communities.owner_id = users.id
            WHERE communities.visibility = 'public' AND communities.deleted_at IS NULL AND communities.owner_id != $1
            GROUP BY communities.id, users.login_name
            HAVING MAX(posts.published_at) IS NOT NULL
            ORDER BY MAX(posts.published_at) DESC
        "#,
        community_owner_id
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn get_user_communities_with_latest_9_posts(
    tx: &mut Transaction<'_, Postgres>,
    community_owner_id: Uuid,
) -> Result<Vec<PublicCommunityWithPosts>> {
    // Select communities ordered by latest published post
    let communities = query_as!(
        PublicCommunity,
        r#"
            SELECT
                communities.id,
                communities.owner_id,
                users.login_name AS owner_login_name,
                communities.name,
                communities.slug,
                communities.description,
                communities.visibility as "visibility: _",
                communities.updated_at,
                communities.created_at,
                communities.foreground_color,
                communities.background_color,
                COUNT(posts.id) AS posts_count
            FROM communities
            LEFT JOIN users ON communities.owner_id = users.id
            LEFT JOIN posts ON communities.id = posts.community_id AND posts.published_at IS NOT NULL AND posts.deleted_at IS NULL
            WHERE communities.owner_id = $1
            AND communities.visibility = 'public' AND communities.deleted_at IS NULL
            GROUP BY communities.id, users.login_name
            ORDER BY MAX(posts.published_at) DESC
            LIMIT 9
        "#,
        community_owner_id
    )
    .fetch_all(&mut **tx)
    .await?;

    // Collect all community IDs for bulk query
    let community_ids: Vec<Uuid> = communities.iter().map(|c| c.id).collect();

    // Fetch all posts for these communities in a single query
    let all_posts = query!(
        r#"
            WITH ranked_posts AS (
                SELECT
                    posts.id,
                    posts.title,
                    posts.author_id,
                    posts.community_id,
                    users.login_name,
                    images.paint_duration AS paint_duration,
                    images.stroke_count AS stroke_count,
                    images.image_filename AS image_filename,
                    images.width AS width,
                    images.height AS height,
                    images.replay_filename AS replay_filename,
                    posts.viewer_count,
                    (posts.is_sensitive OR posts.is_explicit) AS "is_sensitive!",
                    posts.published_at,
                    posts.created_at,
                    posts.updated_at,
                    ROW_NUMBER() OVER (PARTITION BY posts.community_id ORDER BY posts.published_at DESC) as rn
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                WHERE posts.community_id = ANY($1)
                AND posts.deleted_at IS NULL
                AND posts.published_at IS NOT NULL
            )
            SELECT * FROM ranked_posts WHERE rn <= 9
            ORDER BY community_id, published_at DESC
        "#,
        &community_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    // Group posts by community_id
    use std::collections::HashMap;
    let mut posts_by_community: HashMap<Uuid, Vec<SerializablePost>> = HashMap::new();

    for row in all_posts {
        if let Some(community_id) = row.community_id {
            let post = SerializablePost {
                id: row.id,
                title: row.title,
                author_id: row.author_id,
                user_login_name: Some(row.login_name.into()),
                paint_duration: row.paint_duration.microseconds.to_string(),
                stroke_count: row.stroke_count,
                image_filename: row.image_filename,
                image_width: row.width,
                image_height: row.height,
                replay_filename: row.replay_filename,
                is_sensitive: row.is_sensitive,
                viewer_count: row.viewer_count,
                published_at: row.published_at,
                created_at: row.created_at,
                updated_at: row.updated_at,
            };

            posts_by_community
                .entry(community_id)
                .or_default()
                .push(post);
        }
    }

    // Build result with posts attached to each community
    let result = communities
        .into_iter()
        .map(|community| {
            let posts = posts_by_community
                .get(&community.id)
                .cloned()
                .unwrap_or_default();

            PublicCommunityWithPosts {
                id: community.id,
                owner_id: community.owner_id,
                owner_login_name: community.owner_login_name,
                name: community.name,
                slug: community.slug,
                description: community.description,
                visibility: community.visibility,
                updated_at: community.updated_at,
                created_at: community.created_at,
                posts,
            }
        })
        .collect();

    Ok(result)
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct KnownCommunity {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub owner_login_name: LoginName,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub visibility: CommunityVisibility,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
}

pub async fn get_known_communities(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<KnownCommunity>> {
    // Select communities where the user has posted or communities that the user owns, ordered by latest published post
    let q = query_as!(
        KnownCommunity,
        r#"
            SELECT communities.id, communities.owner_id, users.login_name AS owner_login_name, communities.name, communities.slug, communities.description, communities.visibility as "visibility: _", communities.updated_at, communities.created_at, communities.background_color, communities.foreground_color
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id
            LEFT JOIN users ON communities.owner_id = users.id
            WHERE communities.deleted_at IS NULL AND (communities.id IN (
                SELECT DISTINCT community_id
                FROM posts
                WHERE author_id = $1
            ) OR communities.owner_id = $1)
            GROUP BY communities.id, users.login_name
            ORDER BY MAX(posts.published_at) DESC
        "#,
        user_id
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn get_participating_communities(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<Community>> {
    // Select communities that the user is a member of (including private communities), ordered by latest published post
    let q = query_as!(
        Community,
        r#"
            SELECT communities.id, communities.owner_id, communities.name, communities.slug, communities.description, communities.visibility as "visibility: _", communities.updated_at, communities.created_at, communities.background_color, communities.foreground_color
            FROM communities
            LEFT JOIN posts ON communities.id = posts.community_id
            WHERE communities.deleted_at IS NULL AND communities.id IN (
                SELECT DISTINCT community_id
                FROM community_members
                WHERE user_id = $1
            )
            GROUP BY communities.id
            ORDER BY MAX(posts.published_at) DESC NULLS LAST
        "#,
        user_id
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

pub async fn find_community_by_id(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<Community>> {
    let q = query_as!(
        Community,
        r#"SELECT id, owner_id, name, slug, description, visibility as "visibility: _", updated_at, created_at, background_color, foreground_color FROM communities WHERE id = $1 AND deleted_at IS NULL"#,
        id
    );
    Ok(q.fetch_optional(&mut **tx).await?)
}

pub async fn find_community_by_slug(
    tx: &mut Transaction<'_, Postgres>,
    slug: String,
) -> Result<Option<Community>> {
    let q = query_as!(
        Community,
        r#"SELECT id, owner_id, name, slug, description, visibility as "visibility: _", updated_at, created_at, background_color, foreground_color FROM communities WHERE slug = $1 AND deleted_at IS NULL"#,
        slug
    );
    Ok(q.fetch_optional(&mut **tx).await?)
}

/// Check if a slug conflicts with any existing user login_name
pub async fn slug_conflicts_with_user(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
) -> Result<bool> {
    let result = query!(
        "SELECT EXISTS(SELECT 1 FROM users WHERE login_name = $1) as \"exists!\"",
        slug
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(result.exists)
}

pub async fn create_community(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    community_draft: CommunityDraft,
) -> Result<Community> {
    let q = query!(
        r#"
            INSERT INTO communities (
                owner_id,
                name,
                slug,
                description,
                visibility
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, created_at, updated_at
        "#,
        owner_id,
        community_draft.name,
        community_draft.slug,
        community_draft.description,
        community_draft.visibility as _,
    );
    let result = q.fetch_one(&mut **tx).await?;

    // Only add owner as a member for private communities
    // Public and unlisted communities don't require membership
    if community_draft.visibility == CommunityVisibility::Private {
        query!(
            "INSERT INTO community_members (community_id, user_id, role) VALUES ($1, $2, 'owner')",
            result.id,
            owner_id
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(Community {
        id: result.id,
        owner_id,
        name: community_draft.name,
        slug: community_draft.slug,
        description: community_draft.description,
        visibility: community_draft.visibility,
        created_at: result.created_at,
        updated_at: result.updated_at,
        background_color: None,
        foreground_color: None,
    })
}

pub async fn update_community(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    community_draft: CommunityDraft,
    config: Option<&crate::AppConfig>,
) -> Result<Community> {
    // Check if visibility change is allowed (can't change between member_only and public/unlisted)
    let current = find_community_by_id(tx, id).await?;
    if let Some(current_community) = current {
        let is_member_only = |v: CommunityVisibility| v == CommunityVisibility::Private;
        if is_member_only(current_community.visibility)
            != is_member_only(community_draft.visibility)
        {
            return Err(anyhow::anyhow!(
                "Cannot change visibility between member_only and public/unlisted"
            ));
        }
    }

    let q = query!(
        r#"
            UPDATE communities
            SET name = $2, slug = $3, description = $4, visibility = $5, updated_at = now()
            WHERE id = $1
            RETURNING owner_id, created_at
        "#,
        id,
        community_draft.name,
        community_draft.slug,
        community_draft.description,
        community_draft.visibility as _,
    );
    let result = q.fetch_one(&mut **tx).await?;

    // If config is provided, also update the corresponding community actor
    // Only do this for non-member_only communities
    if let Some(config) = config {
        if community_draft.visibility != CommunityVisibility::Private {
            let _ = super::actor::update_actor_for_community(
                tx,
                id,
                community_draft.slug.clone(), // Use slug as username
                community_draft.name.clone(),
                community_draft.description.clone(),
                config,
            )
            .await;
        }
    }

    Ok(Community {
        id,
        owner_id: result.owner_id,
        name: community_draft.name,
        slug: community_draft.slug,
        description: community_draft.description,
        visibility: community_draft.visibility,
        created_at: result.created_at,
        updated_at: Utc::now(),
        background_color: None,
        foreground_color: None,
    })
}

pub async fn update_community_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    community_draft: CommunityDraft,
    config: &crate::AppConfig,
    state: Option<&crate::web::state::AppState>,
) -> Result<Community> {
    // First update the community
    let community = update_community(tx, id, community_draft, Some(config)).await?;

    // If state is provided, send ActivityPub Update activity
    if let Some(state) = state {
        // Get the updated actor
        if let Some(updated_actor) = super::actor::Actor::find_by_community_id(tx, id).await? {
            // Send Update activity - don't fail if this fails
            if let Err(e) =
                crate::web::handlers::activitypub::send_update_activity(&updated_actor, state).await
            {
                tracing::warn!(
                    "Failed to send Update activity for community {}: {:?}",
                    id,
                    e
                );
            }
        }
    }

    Ok(community)
}

// Get communities a user can post to (public communities, unlisted communities, or member_only communities they are a member of)
pub async fn get_communities_for_collaboration(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<Community>> {
    let q = query_as!(
        Community,
        r#"
        SELECT id, owner_id, name, slug, description, visibility as "visibility: _", updated_at, created_at,
               background_color, foreground_color
        FROM communities
        WHERE visibility IN ('public', 'unlisted') OR id IN (
            SELECT community_id FROM community_members WHERE user_id = $1
        )
        ORDER BY name ASC
        "#,
        user_id
    );

    Ok(q.fetch_all(&mut **tx).await?)
}

// Get statistics for a community
pub async fn get_community_stats(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<CommunityStats> {
    let stats = sqlx::query!(
        r#"
        SELECT
            COUNT(DISTINCT CASE WHEN posts.published_at IS NOT NULL AND posts.deleted_at IS NULL THEN posts.id END) AS "total_posts!",
            COUNT(DISTINCT CASE WHEN posts.published_at IS NOT NULL AND posts.deleted_at IS NULL THEN posts.author_id END) AS "total_contributors!",
            COUNT(DISTINCT comments.id) AS "total_comments!"
        FROM communities
        LEFT JOIN posts ON communities.id = posts.community_id
        LEFT JOIN comments ON posts.id = comments.post_id
        WHERE communities.id = $1
        "#,
        community_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(CommunityStats {
        total_posts: stats.total_posts,
        total_contributors: stats.total_contributors,
        total_comments: stats.total_comments,
    })
}

/// Struct for community member stats
pub struct CommunityMembersCount {
    pub community_id: Uuid,
    pub members_count: Option<i64>,
}

/// Fetch members count (unique contributors) for multiple communities
pub async fn get_communities_members_count(
    tx: &mut Transaction<'_, Postgres>,
    community_ids: &[Uuid],
) -> Result<Vec<CommunityMembersCount>> {
    if community_ids.is_empty() {
        return Ok(Vec::new());
    }

    let result = sqlx::query!(
        r#"
        SELECT
            p.community_id,
            COUNT(DISTINCT p.author_id) as members_count
        FROM posts p
        WHERE p.community_id = ANY($1)
            AND p.published_at IS NOT NULL
            AND p.deleted_at IS NULL
        GROUP BY p.community_id
        "#,
        community_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(result
        .into_iter()
        .filter_map(|row| {
            row.community_id.map(|community_id| CommunityMembersCount {
                community_id,
                members_count: row.members_count,
            })
        })
        .collect())
}

// ========== Community Membership Functions ==========

/// Get user's role in a community
pub async fn get_user_role_in_community(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    community_id: Uuid,
) -> Result<Option<CommunityMemberRole>> {
    let result = query_as!(
        CommunityMember,
        r#"
        SELECT id, community_id, user_id, role as "role: _", joined_at, invited_by
        FROM community_members
        WHERE user_id = $1 AND community_id = $2
        "#,
        user_id,
        community_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(result.map(|m| m.role))
}

/// Check if user is a member of a community
pub async fn is_user_member(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    community_id: Uuid,
) -> Result<bool> {
    let result = query!(
        "SELECT EXISTS(SELECT 1 FROM community_members WHERE user_id = $1 AND community_id = $2) as \"exists!\"",
        user_id,
        community_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(result.exists)
}

/// Get all members of a community
pub async fn get_community_members(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<Vec<CommunityMember>> {
    let members = query_as!(
        CommunityMember,
        r#"
        SELECT id, community_id, user_id, role as "role: _", joined_at, invited_by
        FROM community_members
        WHERE community_id = $1
        ORDER BY
            CASE role
                WHEN 'owner' THEN 1
                WHEN 'moderator' THEN 2
                WHEN 'member' THEN 3
            END,
            joined_at ASC
        "#,
        community_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(members)
}

/// Struct for community members with user details (no N+1 query)
#[derive(Debug)]
pub struct CommunityMemberWithDetails {
    pub id: Uuid,
    pub user_id: Uuid,
    pub login_name: LoginName,
    pub display_name: String,
    pub role: CommunityMemberRole,
    pub joined_at: DateTime<Utc>,
}

/// Get community members with user details in a single query
pub async fn get_community_members_with_details(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<Vec<CommunityMemberWithDetails>> {
    let members = query!(
        r#"
        SELECT
            cm.id,
            cm.user_id,
            u.login_name,
            u.display_name,
            cm.role as "role: CommunityMemberRole",
            cm.joined_at
        FROM community_members cm
        JOIN users u ON cm.user_id = u.id
        WHERE cm.community_id = $1
        ORDER BY
            CASE cm.role
                WHEN 'owner' THEN 1
                WHEN 'moderator' THEN 2
                WHEN 'member' THEN 3
            END,
            cm.joined_at ASC
        "#,
        community_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(members
        .into_iter()
        .map(|row| CommunityMemberWithDetails {
            id: row.id,
            user_id: row.user_id,
            login_name: row.login_name.into(),
            display_name: row.display_name,
            role: row.role,
            joined_at: row.joined_at,
        })
        .collect())
}

/// Add a member to a community
pub async fn add_community_member(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    user_id: Uuid,
    role: CommunityMemberRole,
    invited_by: Option<Uuid>,
) -> Result<CommunityMember> {
    let member = query_as!(
        CommunityMember,
        r#"
        INSERT INTO community_members (community_id, user_id, role, invited_by)
        VALUES ($1, $2, $3, $4)
        RETURNING id, community_id, user_id, role as "role: _", joined_at, invited_by
        "#,
        community_id,
        user_id,
        role as _,
        invited_by
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(member)
}

/// Remove a member from a community
pub async fn remove_community_member(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    user_id: Uuid,
) -> Result<()> {
    query!(
        "DELETE FROM community_members WHERE community_id = $1 AND user_id = $2",
        community_id,
        user_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Leave a community (user voluntarily leaves)
/// Returns an error if the user is the owner (owners cannot leave)
pub async fn leave_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    user_id: Uuid,
) -> Result<()> {
    // Check if user is the owner
    let role = get_user_role_in_community(tx, user_id, community_id).await?;

    match role {
        Some(CommunityMemberRole::Owner) => {
            bail!("Owners cannot leave their community. Delete the community instead.");
        }
        Some(_) => {
            // User is a member or moderator, allow them to leave
            remove_community_member(tx, community_id, user_id).await?;
            Ok(())
        }
        None => {
            // User is not a member
            bail!("You are not a member of this community");
        }
    }
}

/// Update a member's role
pub async fn update_member_role(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    user_id: Uuid,
    new_role: CommunityMemberRole,
) -> Result<()> {
    query!(
        "UPDATE community_members SET role = $3 WHERE community_id = $1 AND user_id = $2",
        community_id,
        user_id,
        new_role as _
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

// ========== Community Invitation Functions ==========

/// Create an invitation
pub async fn create_invitation(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    inviter_id: Uuid,
    invitee_id: Uuid,
) -> Result<CommunityInvitation> {
    // Delete any existing invitations for this user to this community
    // This allows re-inviting users who previously accepted/rejected or left the community
    sqlx::query!(
        "DELETE FROM community_invitations WHERE community_id = $1 AND invitee_id = $2",
        community_id,
        invitee_id
    )
    .execute(&mut **tx)
    .await?;

    let invitation = query_as!(
        CommunityInvitation,
        r#"
        INSERT INTO community_invitations (community_id, inviter_id, invitee_id)
        VALUES ($1, $2, $3)
        RETURNING id, community_id, inviter_id, invitee_id, status as "status: _", created_at, updated_at
        "#,
        community_id,
        inviter_id,
        invitee_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(invitation)
}

/// Get pending invitations for a user
pub async fn get_pending_invitations_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<CommunityInvitation>> {
    let invitations = query_as!(
        CommunityInvitation,
        r#"
        SELECT id, community_id, inviter_id, invitee_id, status as "status: _", created_at, updated_at
        FROM community_invitations
        WHERE invitee_id = $1 AND status = 'pending'
        ORDER BY created_at DESC
        "#,
        user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(invitations)
}

/// Struct for invitations with all required details (no N+1 query)
#[derive(Debug)]
pub struct InvitationWithDetails {
    pub id: Uuid,
    pub community_name: String,
    pub community_slug: String,
    pub inviter_login_name: LoginName,
    pub inviter_display_name: String,
    pub created_at: DateTime<Utc>,
}

/// Get pending invitations for a user with all details in a single query
pub async fn get_pending_invitations_with_details_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<InvitationWithDetails>> {
    let invitations = query!(
        r#"
        SELECT
            ci.id,
            c.name as community_name,
            c.slug as community_slug,
            u.login_name as inviter_login_name,
            u.display_name as inviter_display_name,
            ci.created_at
        FROM community_invitations ci
        JOIN communities c ON ci.community_id = c.id
        JOIN users u ON ci.inviter_id = u.id
        WHERE ci.invitee_id = $1 AND ci.status = 'pending'
        ORDER BY ci.created_at DESC
        "#,
        user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(invitations
        .into_iter()
        .map(|row| InvitationWithDetails {
            id: row.id,
            community_name: row.community_name,
            community_slug: row.community_slug,
            inviter_login_name: row.inviter_login_name.into(),
            inviter_display_name: row.inviter_display_name,
            created_at: row.created_at,
        })
        .collect())
}

/// Get pending invitations for a community
pub async fn get_pending_invitations_for_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<Vec<CommunityInvitation>> {
    let invitations = query_as!(
        CommunityInvitation,
        r#"
        SELECT id, community_id, inviter_id, invitee_id, status as "status: _", created_at, updated_at
        FROM community_invitations
        WHERE community_id = $1 AND status = 'pending'
        ORDER BY created_at DESC
        "#,
        community_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(invitations)
}

/// Struct for community invitations with invitee details (no N+1 query)
#[derive(Debug)]
pub struct CommunityInvitationWithInviteeDetails {
    pub id: Uuid,
    pub invitee_login_name: LoginName,
    pub invitee_display_name: String,
    pub created_at: DateTime<Utc>,
}

/// Get pending invitations for a community with invitee details in a single query
pub async fn get_pending_invitations_with_invitee_details_for_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<Vec<CommunityInvitationWithInviteeDetails>> {
    let invitations = query!(
        r#"
        SELECT
            ci.id,
            u.login_name as invitee_login_name,
            u.display_name as invitee_display_name,
            ci.created_at
        FROM community_invitations ci
        JOIN users u ON ci.invitee_id = u.id
        WHERE ci.community_id = $1 AND ci.status = 'pending'
        ORDER BY ci.created_at DESC
        "#,
        community_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(invitations
        .into_iter()
        .map(|row| CommunityInvitationWithInviteeDetails {
            id: row.id,
            invitee_login_name: row.invitee_login_name.into(),
            invitee_display_name: row.invitee_display_name,
            created_at: row.created_at,
        })
        .collect())
}

/// Get invitation by ID
pub async fn get_invitation_by_id(
    tx: &mut Transaction<'_, Postgres>,
    invitation_id: Uuid,
) -> Result<Option<CommunityInvitation>> {
    let invitation = query_as!(
        CommunityInvitation,
        r#"
        SELECT id, community_id, inviter_id, invitee_id, status as "status: _", created_at, updated_at
        FROM community_invitations
        WHERE id = $1
        "#,
        invitation_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(invitation)
}

/// Accept an invitation
pub async fn accept_invitation(
    tx: &mut Transaction<'_, Postgres>,
    invitation_id: Uuid,
) -> Result<()> {
    query!(
        r#"
        UPDATE community_invitations
        SET status = 'accepted', updated_at = now()
        WHERE id = $1 AND status = 'pending'
        "#,
        invitation_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Reject an invitation
pub async fn reject_invitation(
    tx: &mut Transaction<'_, Postgres>,
    invitation_id: Uuid,
) -> Result<()> {
    query!(
        r#"
        UPDATE community_invitations
        SET status = 'rejected', updated_at = now()
        WHERE id = $1 AND status = 'pending'
        "#,
        invitation_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Soft delete a community and all its posts
pub async fn soft_delete_community(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
    user_id: Uuid,
) -> Result<()> {
    // First, verify the community exists and user is the owner
    let community = query!(
        r#"
        SELECT id, owner_id, deleted_at
        FROM communities
        WHERE slug = $1
        "#,
        slug
    )
    .fetch_optional(&mut **tx)
    .await?;

    let community = community.ok_or_else(|| anyhow::anyhow!("Community not found"))?;

    // Check if already deleted
    if community.deleted_at.is_some() {
        return Err(anyhow::anyhow!("Community already deleted"));
    }

    // Verify ownership
    if community.owner_id != user_id {
        return Err(anyhow::anyhow!("Only the owner can delete this community"));
    }

    // Soft delete the community
    query!(
        r#"
        UPDATE communities
        SET deleted_at = now()
        WHERE id = $1
        "#,
        community.id
    )
    .execute(&mut **tx)
    .await?;

    // Soft delete all posts in the community with cascade reason
    crate::models::post::soft_delete_community_posts(tx, community.id).await?;

    Ok(())
}

pub async fn soft_delete_community_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
    user_id: Uuid,
    _config: &crate::config::AppConfig,
    state: Option<&crate::web::state::AppState>,
) -> Result<()> {
    // First, get the community to find its ID
    let community = query!(
        r#"
        SELECT id
        FROM communities
        WHERE slug = $1
        "#,
        slug
    )
    .fetch_optional(&mut **tx)
    .await?;

    let community_id = community
        .ok_or_else(|| anyhow::anyhow!("Community not found"))?
        .id;

    // Get the community's actor before deletion
    let actor = super::actor::Actor::find_by_community_id(tx, community_id).await?;

    // Soft delete the community
    soft_delete_community(tx, slug, user_id).await?;

    // If state is provided and actor exists, send ActivityPub Delete activity
    if let (Some(state), Some(actor)) = (state, actor) {
        // Use the actor's IRI as the object URL
        if let Ok(actor_url) = actor.iri.parse() {
            // Send Delete activity - don't fail if this fails
            if let Err(e) =
                crate::web::handlers::activitypub::send_delete_activity(&actor, actor_url, state)
                    .await
            {
                tracing::warn!(
                    "Failed to send Delete activity for community {}: {:?}",
                    slug,
                    e
                );
            }
        }
    }

    Ok(())
}
