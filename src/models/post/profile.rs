//! A person's drawings, as their profile lists them.

use anyhow::Result;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use crate::models::community::CommunityVisibility;

use super::{SerializablePostForHome, SerializableProfilePost};

pub async fn find_published_public_posts_by_author_id(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<SerializableProfilePost>> {
    let q = query!(
        r#"
            SELECT
                posts.id,
                posts.author_id,
                posts.title,
                posts.viewer_count,
                images.paint_duration,
                images.stroke_count,
                images.image_filename,
                images.width,
                images.height,
                images.replay_filename,
                posts.published_at,
                posts.created_at,
                posts.updated_at,
                communities.visibility as "visibility?: CommunityVisibility",
                communities.slug as "community_slug?"
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE author_id = $1
            AND (communities.visibility = 'public' OR posts.community_id IS NULL)
            AND published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            ORDER BY published_at DESC
            LIMIT $2 OFFSET $3
        "#,
        author_id,
        limit,
        offset
    );
    let result = q.fetch_all(&mut **tx).await?;
    Ok(result
        .into_iter()
        .map(|row| SerializableProfilePost {
            id: row.id,
            title: row.title,
            viewer_count: row.viewer_count,
            author_id: row.author_id,
            paint_duration: row.paint_duration.microseconds.to_string(),
            stroke_count: row.stroke_count,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            replay_filename: row.replay_filename,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            community_visibility: row.visibility,
            community_slug: row.community_slug,
        })
        .collect())
}

/// Which of a person's drawings a profile tab shows: those anyone can see --
/// outside any community, or in a public one -- or those only they see on
/// their own profile, in communities that are unlisted or private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileDrawings {
    Public,
    Private,
}

/// A batch of a person's published drawings for a profile tab, newest
/// first, as the shared feed card draws them (post_card.jinja). Sensitive
/// drawings follow the feeds' rule: shown to a viewer who shows them, and
/// to the artist, and blurred by the card either way.
pub async fn find_profile_posts(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    which: ProfileDrawings,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<SerializablePostForHome>> {
    let private = which == ProfileDrawings::Private;
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
            JOIN images ON posts.image_id = images.id
            JOIN users ON posts.author_id = users.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.author_id = $1
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND (CASE WHEN $2
                THEN posts.community_id IS NOT NULL AND communities.visibility <> 'public'
                ELSE posts.community_id IS NULL OR communities.visibility = 'public'
            END)
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $3 = true OR posts.author_id = $4)
            ORDER BY posts.published_at DESC
            LIMIT $5 OFFSET $6
        ",
        author_id,
        private,
        viewer_show_sensitive,
        viewer_user_id,
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

/// How many drawings `find_profile_posts` pages through, for the tab's
/// count: the same filter, so the number is what the tab shows.
pub async fn count_profile_posts(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    which: ProfileDrawings,
    viewer_user_id: Option<Uuid>,
    viewer_show_sensitive: bool,
) -> Result<i64> {
    let private = which == ProfileDrawings::Private;
    let count = sqlx::query_scalar!(
        r#"
            SELECT COUNT(*) AS "count!"
            FROM posts
            JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.author_id = $1
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND (CASE WHEN $2
                THEN posts.community_id IS NOT NULL AND communities.visibility <> 'public'
                ELSE posts.community_id IS NULL OR communities.visibility = 'public'
            END)
            AND ((posts.is_sensitive = false AND posts.is_explicit = false) OR $3 = true OR posts.author_id = $4)
        "#,
        author_id,
        private,
        viewer_show_sensitive,
        viewer_user_id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(count)
}
