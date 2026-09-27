//! Deleting posts, and telling other servers they are gone.

use anyhow::Result;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use crate::models::notification::BadgeFalls;

use super::{find_post_by_id, PostDeletionReason};

pub async fn delete_post(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    reason: PostDeletionReason,
) -> Result<BadgeFalls> {
    let q = query!(
        "
        UPDATE posts
        SET
            deleted_at = now(),
            deletion_reason = $2,
            title = NULL,
            content = NULL
        WHERE id = $1
        RETURNING image_id
    ",
        id,
        reason as PostDeletionReason
    )
    .fetch_one(&mut **tx)
    .await?;

    println!("image_id: {:?}", q.image_id);

    query!(
        "
        UPDATE images
        SET deleted_at = now()
        WHERE id = $1
        ",
        q.image_id
    )
    .execute(&mut **tx)
    .await?;

    // Delete notifications referencing this post
    let retracted = query!(
        r#"
        DELETE FROM notifications
        WHERE post_id = $1
        RETURNING id, recipient_id, read_at IS NULL AS "unread!"
        "#,
        id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(retracted
        .into_iter()
        .filter(|row| row.unread)
        .map(|row| BadgeFalls::withdrawn(row.recipient_id, row.id))
        .collect())
}

pub async fn soft_delete_community_posts(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
) -> Result<BadgeFalls> {
    query!(
        "
        UPDATE posts
        SET
            deleted_at = now(),
            deletion_reason = 'cascade',
            title = NULL,
            content = NULL
        WHERE community_id = $1
          AND deleted_at IS NULL
        ",
        community_id
    )
    .execute(&mut **tx)
    .await?;

    // Note: We do NOT delete images or R2 objects for community cascade deletions
    // This allows for potential recovery if the community is restored

    // Delete notifications for posts in this community
    let retracted = query!(
        r#"
        DELETE FROM notifications
        WHERE post_id IN (
            SELECT id FROM posts WHERE community_id = $1
        )
        RETURNING id, recipient_id, read_at IS NULL AS "unread!"
        "#,
        community_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(retracted
        .into_iter()
        .filter(|row| row.unread)
        .map(|row| BadgeFalls::withdrawn(row.recipient_id, row.id))
        .collect())
}

pub async fn delete_post_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    app_state: Option<&crate::web::state::AppState>,
) -> Result<BadgeFalls> {
    // First, get post details before deletion
    let post = find_post_by_id(tx, id).await?;
    let post = match post {
        Some(post) => post,
        None => return Err(anyhow::anyhow!("Post not found")),
    };

    // Get the author's actor
    let author_id_str = post
        .get("author_id")
        .and_then(|v| v.as_ref())
        .ok_or_else(|| anyhow::anyhow!("Post has no author"))?;
    let author_id = uuid::Uuid::parse_str(author_id_str)?;

    // Perform the deletion
    let falls = delete_post(tx, id, PostDeletionReason::UserDeleted).await?;

    // If app_state is provided, send ActivityPub Delete activity
    if let Some(state) = app_state {
        // Get the author's actor
        if let Some(author_actor) =
            crate::models::actor::Actor::find_by_user_id(tx, author_id).await?
        {
            // Create the object URL that was deleted
            let object_url = format!("https://{}/ap/posts/{}", state.config.domain, id);
            let object_url = object_url.parse()?;

            // Send Delete activity - don't fail if this fails
            if let Err(e) = crate::web::handlers::activitypub::send_delete_activity(
                &author_actor,
                object_url,
                state,
            )
            .await
            {
                tracing::warn!("Failed to send Delete activity for post {}: {:?}", id, e);
            }
        }
    }

    Ok(falls)
}
