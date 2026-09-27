//! Drawings on their way to being posts: drafts, and publishing them.

use anyhow::Result;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use super::{PostDraft, SerializableDraftPost, SerializablePost};

/// The post an earlier upload of the same locally saved drawing made, if one
/// did. A drawing is kept on the device before it is sent, so it can arrive
/// twice -- from two tabs, or after a tab closed between the upload landing and
/// its local copy being forgotten.
pub async fn find_post_id_by_client_draft_id(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    client_draft_id: Uuid,
) -> Result<Option<Uuid>> {
    let row = query!(
        "SELECT id FROM posts WHERE author_id = $1 AND client_draft_id = $2 AND deleted_at IS NULL",
        author_id,
        client_draft_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|row| row.id))
}

pub async fn get_draft_post_count(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
) -> Result<i64> {
    let q = query!(
        "
            SELECT COUNT(*)
            FROM posts
            WHERE author_id = $1
            AND published_at IS NULL
            AND deleted_at IS NULL 
        ",
        author_id
    );
    let result = q.fetch_one(&mut **tx).await?;
    Ok(result.count.unwrap_or(0))
}

pub async fn find_draft_posts_by_author_id(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
) -> Result<Vec<SerializableDraftPost>> {
    let result = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.content,
                posts.community_id,
                posts.updated_at,
                images.image_filename,
                images.width,
                images.height,
                communities.name as \"community_name?\"
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.author_id = $1
            AND posts.published_at IS NULL
            AND posts.deleted_at IS NULL
            ORDER BY posts.updated_at DESC
        ",
        author_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(result
        .into_iter()
        .map(|row| SerializableDraftPost {
            id: row.id,
            title: row.title,
            content: row.content,
            community_id: row.community_id,
            community_name: row.community_name,
            image_filename: row.image_filename,
            image_width: row.width,
            image_height: row.height,
            updated_at: row.updated_at,
        })
        .collect())
}

pub async fn create_post(
    tx: &mut Transaction<'_, Postgres>,
    post_draft: PostDraft,
) -> Result<SerializablePost> {
    let image = query!(
        r#"
            INSERT INTO images (
                paint_duration,
                stroke_count,
                width,
                height,
                image_filename,
                replay_filename,
                tool
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
        "#,
        post_draft.paint_duration,
        post_draft.stroke_count,
        post_draft.width,
        post_draft.height,
        post_draft.image_filename,
        post_draft.replay_filename,
        post_draft.tool as _
    )
    .fetch_one(&mut **tx)
    .await?;

    let post = query!(
        "
            INSERT INTO posts (
                author_id,
                image_id,
                community_id,
                is_sensitive,
                parent_post_id,
                client_draft_id
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, created_at, updated_at
        ",
        post_draft.author_id,
        image.id,
        post_draft.community_id,
        false,
        post_draft.parent_post_id,
        post_draft.client_draft_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(SerializablePost {
        id: post.id,
        title: None,
        author_id: post_draft.author_id,
        user_login_name: None,
        paint_duration: post_draft.paint_duration.microseconds.to_string(),
        stroke_count: post_draft.stroke_count,
        image_filename: post_draft.image_filename,
        image_width: post_draft.width,
        image_height: post_draft.height,
        replay_filename: post_draft.replay_filename,
        is_sensitive: false,
        viewer_count: 0,
        published_at: None,
        created_at: post.created_at,
        updated_at: post.updated_at,
    })
}

pub async fn publish_post(
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
                -- Keep the first publication time. Nothing stopped the publish
                -- form being submitted twice, and the second one used to move
                -- the drawing back to the top of every feed.
                published_at = COALESCE(published_at, now()),
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
