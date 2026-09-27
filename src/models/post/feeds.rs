//! The lists of posts the site shows: public, followed, a community's, collaborative.

use crate::models::handle::LoginName;
use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use super::{SerializablePost, SerializablePostForHome};

pub async fn find_posts_by_community_id(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<Vec<SerializablePost>> {
    let q = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.author_id,
                images.paint_duration AS paint_duration,
                images.stroke_count AS stroke_count,
                images.image_filename AS image_filename,
                images.width AS width,
                images.height AS height,
                images.replay_filename AS replay_filename,
                (posts.is_sensitive OR posts.is_explicit) AS \"is_sensitive!\",
                posts.viewer_count,
                posts.published_at,
                posts.created_at,
                posts.updated_at
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            WHERE community_id = $1
            AND posts.deleted_at IS NULL
        ",
        community_id
    );
    let result = q.fetch_all(&mut **tx).await?;
    Ok(result
        .into_iter()
        .map(|row| SerializablePost {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: None,
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
        })
        .collect())
}

/// One community's published drawings, newest first.
///
/// Returns the same row shape as the home feed so a community page can render
/// the shared post card: the card credits each drawing to its author and its
/// community, and both live here rather than being stitched on by the caller.
pub async fn find_published_posts_by_community_id(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    limit: i64,
    offset: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<SerializablePostForHome>> {
    let q = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.author_id,
                users.login_name,
                images.paint_duration,
                images.stroke_count,
                images.image_filename,
                images.width,
                images.height,
                images.replay_filename,
                posts.viewer_count,
                (posts.is_sensitive OR posts.is_explicit) AS \"is_sensitive!\",
                communities.slug AS community_slug,
                communities.name AS community_name,
                posts.published_at,
                posts.created_at,
                posts.updated_at
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN users ON posts.author_id = users.id
            JOIN communities ON posts.community_id = communities.id
            WHERE community_id = $1
            AND published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $4 = true OR posts.author_id = $5)
            ORDER BY published_at DESC
            LIMIT $2 OFFSET $3
        ",
        community_id,
        limit,
        offset,
        viewer_show_sensitive,
        viewer_user_id
    );
    let result = q.fetch_all(&mut **tx).await?;
    Ok(result
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: Some(row.community_slug),
            community_name: Some(row.community_name),
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
        .collect())
}

/// Struct for recent post thumbnails in community cards
pub struct CommunityRecentPost {
    pub id: Uuid,
    pub community_id: Option<Uuid>,
    pub image_filename: String,
    pub image_width: i32,
    pub image_height: i32,
    pub author_login_name: LoginName,
    pub published_at: Option<DateTime<Utc>>,
}

/// Fetch recent posts (up to `limit` per community) for multiple communities
pub async fn find_recent_posts_by_communities(
    tx: &mut Transaction<'_, Postgres>,
    community_ids: &[Uuid],
    limit: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<CommunityRecentPost>> {
    if community_ids.is_empty() {
        return Ok(Vec::new());
    }

    let result = sqlx::query!(
        r#"
        SELECT
            ranked.id,
            ranked.community_id,
            ranked.image_filename,
            ranked.image_width,
            ranked.image_height,
            ranked.author_login_name,
            ranked.published_at
        FROM (
            SELECT
                p.id,
                p.community_id,
                p.author_id,
                i.image_filename,
                i.width as image_width,
                i.height as image_height,
                u.login_name as author_login_name,
                p.published_at,
                ROW_NUMBER() OVER (PARTITION BY p.community_id ORDER BY p.published_at DESC) as rn
            FROM posts p
            INNER JOIN images i ON p.image_id = i.id
            INNER JOIN users u ON p.author_id = u.id
            WHERE p.community_id = ANY($1)
                AND p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
                AND ((p.is_sensitive = false AND p.is_explicit = false) OR $3 = true OR p.author_id = $4)
        ) ranked
        WHERE ranked.rn <= $2
        ORDER BY ranked.community_id, ranked.rn
        "#,
        community_ids,
        limit,
        viewer_show_sensitive,
        viewer_user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(result
        .into_iter()
        .map(|row| CommunityRecentPost {
            id: row.id,
            community_id: row.community_id,
            image_filename: row.image_filename,
            image_width: row.image_width,
            image_height: row.image_height,
            author_login_name: row.author_login_name.into(),
            published_at: row.published_at,
        })
        .collect())
}

/// The public feed behind `/` and `/api/home/posts`.
///
/// Drawings saved out of a collaborative session are left out: they have their
/// own lobby (see `find_collaborative_posts`), and a session that ends with
/// several people saving the same canvas would otherwise fill the front page
/// with near-identical cards.
pub async fn find_public_posts(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
    offset: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<SerializablePostForHome>> {
    let q = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.author_id,
                users.login_name,
                images.paint_duration,
                images.stroke_count,
                images.image_filename,
                images.width,
                images.height,
                images.replay_filename,
                posts.viewer_count,
                (posts.is_sensitive OR posts.is_explicit) AS \"is_sensitive!\",
                communities.slug AS \"community_slug?\",
                communities.name AS \"community_name?\",
                posts.published_at,
                posts.created_at,
                posts.updated_at
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            LEFT JOIN users ON posts.author_id = users.id
            WHERE (communities.visibility = 'public' OR posts.community_id IS NULL)
            AND posts.parent_post_id IS NULL
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $3 = true OR posts.author_id = $4)
            AND NOT EXISTS (
                SELECT 1 FROM collaborative_sessions cs WHERE cs.saved_post_id = posts.id
            )
            ORDER BY posts.published_at DESC
            LIMIT $1
            OFFSET $2
        ",
        limit,
        offset,
        viewer_show_sensitive,
        viewer_user_id
    );
    let result = q.fetch_all(&mut **tx).await?;
    Ok(result
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: row.community_slug,
            community_name: row.community_name,
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
        .collect())
}

/// New drawings in the communities the viewer is a member of -- Home's
/// Communities. Whatever the community's visibility, since a member reads its
/// drawings on its own page anyway; otherwise the public feed's rules, newest
/// first.
pub async fn find_member_community_posts(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    viewer_show_sensitive: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<SerializablePostForHome>> {
    let result = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.author_id,
                users.login_name,
                images.paint_duration,
                images.stroke_count,
                images.image_filename,
                images.width,
                images.height,
                images.replay_filename,
                posts.viewer_count,
                (posts.is_sensitive OR posts.is_explicit) AS \"is_sensitive!\",
                communities.slug AS \"community_slug?\",
                communities.name AS \"community_name?\",
                posts.published_at,
                posts.created_at,
                posts.updated_at
            FROM posts
            JOIN community_members
                ON community_members.community_id = posts.community_id
                AND community_members.user_id = $1
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            LEFT JOIN users ON posts.author_id = users.id
            WHERE posts.parent_post_id IS NULL
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $2 = true OR posts.author_id = $1)
            AND NOT EXISTS (
                SELECT 1 FROM collaborative_sessions cs WHERE cs.saved_post_id = posts.id
            )
            ORDER BY posts.published_at DESC
            LIMIT $3 OFFSET $4
        ",
        user_id,
        viewer_show_sensitive,
        limit,
        offset
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(result
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: row.community_slug,
            community_name: row.community_name,
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
        .collect())
}

/// Finished collaborative drawings — the posts saved sessions turned into.
/// Same shape as `find_public_posts`, so the collaborate lobby renders through
/// the shared feed card and paginates through the shared sentinel.
pub async fn find_collaborative_posts(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
    offset: i64,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<Vec<SerializablePostForHome>> {
    let result = query!(
        r#"
        SELECT
            p.id,
            p.title,
            p.author_id,
            u.login_name,
            i.paint_duration,
            i.stroke_count,
            i.image_filename,
            i.width,
            i.height,
            i.replay_filename,
            p.viewer_count,
            (p.is_sensitive OR p.is_explicit) AS "is_sensitive!",
            c.slug AS "community_slug?",
            c.name AS "community_name?",
            p.published_at,
            p.created_at,
            p.updated_at
        FROM collaborative_sessions cs
        JOIN posts p ON cs.saved_post_id = p.id
        JOIN users u ON p.author_id = u.id
        JOIN images i ON p.image_id = i.id
        LEFT JOIN communities c ON p.community_id = c.id
        WHERE p.published_at IS NOT NULL
          AND p.deleted_at IS NULL
          AND (c.visibility = 'public' OR p.community_id IS NULL)
          AND ((p.is_sensitive = false AND p.is_explicit = false)
               OR $3 = true
               OR p.author_id = $4)
        ORDER BY p.published_at DESC
        LIMIT $1
        OFFSET $2
        "#,
        limit,
        offset,
        viewer_show_sensitive,
        viewer_user_id,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(result
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: row.community_slug,
            community_name: row.community_name,
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
        .collect())
}

/// Posts from people the viewer follows. Returns the same shape as
/// `find_public_posts` so the timeline renders through the shared feed card,
/// community label and all.
pub async fn find_following_posts_by_user_id(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    viewer_show_sensitive: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<SerializablePostForHome>> {
    let q = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.author_id,
                users.login_name,
                images.paint_duration,
                images.stroke_count,
                images.image_filename,
                images.width,
                images.height,
                images.replay_filename,
                posts.viewer_count,
                (posts.is_sensitive OR posts.is_explicit) AS \"is_sensitive!\",
                communities.slug AS \"community_slug?\",
                communities.name AS \"community_name?\",
                posts.published_at,
                posts.created_at,
                posts.updated_at
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN users ON posts.author_id = users.id
            LEFT JOIN actors author_actor ON posts.author_id = author_actor.user_id
            LEFT JOIN follows ON author_actor.id = follows.following_actor_id
            LEFT JOIN actors follower_actor ON follows.follower_actor_id = follower_actor.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE follower_actor.user_id = $1
            AND communities.visibility = 'public'
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $2 = true OR posts.author_id = $1)
            ORDER BY posts.published_at DESC
            LIMIT $3 OFFSET $4
        ",
        user_id,
        viewer_show_sensitive,
        limit,
        offset
    );
    let result = q.fetch_all(&mut **tx).await?;
    Ok(result
        .into_iter()
        .map(|row| SerializablePostForHome {
            id: row.id,
            title: row.title,
            author_id: row.author_id,
            user_login_name: row.login_name.into(),
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            is_sensitive: row.is_sensitive,
            community_slug: row.community_slug,
            community_name: row.community_name,
            viewer_count: row.viewer_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
        .collect())
}
