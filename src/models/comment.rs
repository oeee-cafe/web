use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction, Type};
use uuid::Uuid;

type CommentData = (
    Uuid,                  // post_id
    Uuid,                  // actor_id
    Option<Uuid>,          // parent_comment_id
    Option<String>,        // content
    Option<String>,        // content_html
    Option<String>,        // iri
    String,                // actor_name
    String,                // actor_handle
    String,                // actor_url
    Option<String>,        // actor_login_name
    bool,                  // is_local
    DateTime<Utc>,         // updated_at
    DateTime<Utc>,         // created_at
    Option<DateTime<Utc>>, // deleted_at
);

#[derive(Clone, Debug, Serialize, Type)]
#[sqlx(type_name = "comment_deletion_reason", rename_all = "snake_case")]
pub enum CommentDeletionReason {
    UserDeleted,
    Moderation,
    Cascade,
}

#[derive(Clone, Debug, Serialize)]
pub struct Comment {
    pub id: Uuid,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub content: Option<String>,
    pub content_html: Option<String>,
    pub iri: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

pub struct CommentDraft {
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub content: String,
    pub content_html: Option<String>,
}

#[derive(Serialize)]
pub struct SerializableComment {
    pub id: Uuid,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub content: Option<String>,
    pub content_html: Option<String>,
    pub iri: Option<String>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_url: String,
    pub actor_login_name: Option<String>,
    pub is_local: bool,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct SerializableThreadedComment {
    pub id: Uuid,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub content: Option<String>,
    pub content_html: Option<String>,
    pub iri: Option<String>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_url: String,
    pub actor_login_name: Option<String>,
    pub is_local: bool,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub children: Vec<SerializableThreadedComment>,
}

#[derive(Serialize)]
pub struct NotificationComment {
    pub id: Uuid,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub content: Option<String>,
    pub content_html: Option<String>,
    pub iri: Option<String>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_url: String,
    pub actor_login_name: Option<String>,
    pub is_local: bool,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub post_title: Option<String>,
    pub post_author_login_name: String,
    pub post_image_filename: Option<String>,
    pub post_image_width: Option<i32>,
    pub post_image_height: Option<i32>,
}

pub async fn find_comments_by_post_id(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Vec<SerializableComment>> {
    let comments = sqlx::query!(
        r#"
        SELECT
            comments.id,
            comments.post_id,
            comments.actor_id,
            comments.updated_at,
            comments.created_at,
            comments.content,
            comments.content_html,
            comments.iri,
            actors.name AS actor_name,
            actors.handle AS actor_handle,
            actors.url AS actor_url,
            users.login_name AS "user_login_name?"
        FROM comments
        LEFT JOIN actors ON comments.actor_id = actors.id
        LEFT JOIN users ON actors.user_id = users.id
        WHERE post_id = $1
        AND comments.deleted_at IS NULL
        ORDER BY created_at ASC
        "#,
        post_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(comments
        .into_iter()
        .map(|comment| {
            let is_local = comment.user_login_name.is_some();
            SerializableComment {
                id: comment.id,
                post_id: comment.post_id,
                actor_id: comment.actor_id,
                content: comment.content,
                content_html: comment.content_html,
                iri: comment.iri,
                actor_name: comment.actor_name,
                actor_handle: comment.actor_handle,
                actor_url: comment.actor_url,
                actor_login_name: comment.user_login_name.clone(),
                is_local,
                updated_at: comment.updated_at,
                created_at: comment.created_at,
            }
        })
        .collect())
}

pub async fn build_comment_thread_tree(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Vec<SerializableThreadedComment>> {
    use std::collections::HashMap;

    // Use recursive CTE to fetch all comments with their parent relationships
    let rows = sqlx::query!(
        r#"
        WITH RECURSIVE comment_tree AS (
            -- Base case: all comments for this post
            SELECT
                comments.id,
                comments.post_id,
                comments.actor_id,
                comments.parent_comment_id,
                comments.content,
                comments.content_html,
                comments.iri,
                comments.updated_at,
                comments.created_at,
                comments.deleted_at,
                actors.name AS actor_name,
                actors.handle AS actor_handle,
                actors.url AS actor_url,
                users.login_name AS user_login_name
            FROM comments
            LEFT JOIN actors ON comments.actor_id = actors.id
            LEFT JOIN users ON actors.user_id = users.id
            WHERE comments.post_id = $1
        )
        SELECT
            id,
            post_id,
            actor_id,
            parent_comment_id,
            content AS "content?",
            content_html AS "content_html?",
            iri AS "iri?",
            updated_at,
            created_at,
            deleted_at,
            actor_name AS "actor_name?",
            actor_handle AS "actor_handle?",
            actor_url AS "actor_url?",
            user_login_name AS "user_login_name?"
        FROM comment_tree
        ORDER BY created_at ASC
        "#,
        post_id
    )
    .fetch_all(&mut **tx)
    .await?;

    // Build maps for efficient tree construction
    let mut comment_data: HashMap<Uuid, CommentData> = HashMap::new();

    let mut children_map: HashMap<Option<Uuid>, Vec<Uuid>> = HashMap::new();

    for row in rows {
        let comment_id = row.id;
        // user_login_name can be NULL from LEFT JOIN
        let user_login_name = row.user_login_name;
        let is_local = user_login_name.is_some();

        comment_data.insert(
            comment_id,
            (
                row.post_id,
                row.actor_id,
                row.parent_comment_id,
                row.content,
                row.content_html,
                row.iri,
                row.actor_name.unwrap_or_default(),
                row.actor_handle.unwrap_or_default(),
                row.actor_url.unwrap_or_default(),
                user_login_name,
                is_local,
                row.updated_at,
                row.created_at,
                row.deleted_at,
            ),
        );

        children_map
            .entry(row.parent_comment_id)
            .or_default()
            .push(comment_id);
    }

    // Recursive function to build subtree
    fn build_subtree(
        comment_id: Uuid,
        comment_data: &HashMap<Uuid, CommentData>,
        children_map: &HashMap<Option<Uuid>, Vec<Uuid>>,
    ) -> Option<SerializableThreadedComment> {
        let (
            post_id,
            actor_id,
            parent_comment_id,
            content,
            content_html,
            iri,
            actor_name,
            actor_handle,
            actor_url,
            actor_login_name,
            is_local,
            updated_at,
            created_at,
            deleted_at,
        ) = comment_data.get(&comment_id)?;

        let children = children_map
            .get(&Some(comment_id))
            .map(|child_ids| {
                child_ids
                    .iter()
                    .filter_map(|child_id| build_subtree(*child_id, comment_data, children_map))
                    .collect()
            })
            .unwrap_or_default();

        Some(SerializableThreadedComment {
            id: comment_id,
            post_id: *post_id,
            actor_id: *actor_id,
            parent_comment_id: *parent_comment_id,
            content: content.clone(),
            content_html: content_html.clone(),
            iri: iri.clone(),
            actor_name: actor_name.clone(),
            actor_handle: actor_handle.clone(),
            actor_url: actor_url.clone(),
            actor_login_name: actor_login_name.clone(),
            is_local: *is_local,
            updated_at: *updated_at,
            created_at: *created_at,
            deleted_at: *deleted_at,
            children,
        })
    }

    // Build trees for all root comments (comments with no parent)
    let result: Vec<SerializableThreadedComment> = children_map
        .get(&None)
        .map(|root_ids| {
            root_ids
                .iter()
                .filter_map(|comment_id| build_subtree(*comment_id, &comment_data, &children_map))
                .collect()
        })
        .unwrap_or_default();

    Ok(result)
}

pub async fn find_comments_to_posts_by_author(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
) -> Result<Vec<NotificationComment>> {
    let comments = sqlx::query_as!(
        NotificationComment,
        r#"
        SELECT
            comments.id,
            comments.post_id,
            comments.actor_id,
            comments.updated_at,
            comments.created_at,
            comments.content,
            comments.content_html,
            comments.iri,
            actors.name AS actor_name,
            actors.handle AS actor_handle,
            actors.url AS actor_url,
            comment_authors.login_name AS "actor_login_name?",
            CASE WHEN comment_authors.id IS NOT NULL THEN true ELSE false END AS "is_local!",
            posts.title AS post_title,
            post_authors.login_name AS post_author_login_name,
            images.image_filename AS post_image_filename,
            images.width AS post_image_width,
            images.height AS post_image_height
        FROM comments
        LEFT JOIN actors ON comments.actor_id = actors.id
        LEFT JOIN users AS comment_authors ON actors.user_id = comment_authors.id
        LEFT JOIN posts ON comments.post_id = posts.id
        LEFT JOIN users AS post_authors ON posts.author_id = post_authors.id
        LEFT JOIN images ON posts.image_id = images.id
        WHERE posts.author_id = $1
        AND actors.user_id != $1
        AND posts.deleted_at IS NULL
        AND comments.deleted_at IS NULL
        ORDER BY created_at DESC
        "#,
        author_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(comments)
}

pub async fn find_latest_comments_in_community(
    tx: &mut Transaction<'_, Postgres>,
    community_id: Uuid,
    limit: i64,
) -> Result<Vec<NotificationComment>> {
    let comments = sqlx::query_as!(
        NotificationComment,
        r#"
        SELECT
            comments.id,
            comments.post_id,
            comments.actor_id,
            comments.updated_at,
            comments.created_at,
            comments.content,
            comments.content_html,
            comments.iri,
            actors.name AS actor_name,
            actors.handle AS actor_handle,
            actors.url AS actor_url,
            comment_authors.login_name AS "actor_login_name?",
            CASE WHEN comment_authors.id IS NOT NULL THEN true ELSE false END AS "is_local!",
            posts.title AS post_title,
            post_authors.login_name AS post_author_login_name,
            images.image_filename AS post_image_filename,
            images.width AS post_image_width,
            images.height AS post_image_height
        FROM comments
        LEFT JOIN actors ON comments.actor_id = actors.id
        LEFT JOIN users AS comment_authors ON actors.user_id = comment_authors.id
        LEFT JOIN posts ON comments.post_id = posts.id
        LEFT JOIN users AS post_authors ON posts.author_id = post_authors.id
        LEFT JOIN images ON posts.image_id = images.id
        WHERE posts.community_id = $1
        AND posts.published_at IS NOT NULL
        AND (actors.user_id IS NULL OR actors.user_id != posts.author_id)
        AND posts.deleted_at IS NULL
        AND comments.deleted_at IS NULL
        ORDER BY comments.created_at DESC
        LIMIT $2
        "#,
        community_id,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(comments)
}

pub async fn create_comment(
    tx: &mut Transaction<'_, Postgres>,
    draft: CommentDraft,
) -> Result<Comment> {
    let comment = sqlx::query_as!(
        Comment,
        r#"
        INSERT INTO comments (post_id, actor_id, parent_comment_id, content, content_html)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, post_id, actor_id, parent_comment_id, content, content_html, iri, created_at, updated_at, deleted_at
        "#,
        draft.post_id,
        draft.actor_id,
        draft.parent_comment_id,
        draft.content,
        draft.content_html
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(comment)
}

pub async fn find_comment_by_iri(
    tx: &mut Transaction<'_, Postgres>,
    iri: &str,
) -> Result<Option<Comment>> {
    let comment = sqlx::query_as!(
        Comment,
        r#"
        SELECT id, post_id, actor_id, parent_comment_id, content, content_html, iri, created_at, updated_at, deleted_at
        FROM comments
        WHERE iri = $1
        AND deleted_at IS NULL
        "#,
        iri
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(comment)
}

pub async fn delete_comment_by_iri(tx: &mut Transaction<'_, Postgres>, iri: &str) -> Result<bool> {
    // First find the comment to get its ID
    let comment = sqlx::query!(
        r#"
        SELECT id FROM comments
        WHERE iri = $1
        "#,
        iri
    )
    .fetch_optional(&mut **tx)
    .await?;

    match comment {
        Some(c) => {
            // Soft delete using the delete_comment function
            delete_comment(tx, c.id, CommentDeletionReason::Moderation).await?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub async fn create_comment_from_activitypub(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    actor_id: Uuid,
    content: String,
    content_html: Option<String>,
    iri: String,
) -> Result<Comment> {
    let comment = sqlx::query_as!(
        Comment,
        r#"
        INSERT INTO comments (post_id, actor_id, parent_comment_id, content, content_html, iri)
        VALUES ($1, $2, NULL, $3, $4, $5)
        RETURNING id, post_id, actor_id, parent_comment_id, content, content_html, iri, created_at, updated_at, deleted_at
        "#,
        post_id,
        actor_id,
        content,
        content_html,
        iri
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(comment)
}

/// Extract @mentions from comment content
/// Returns a list of login names (without the @ prefix)
pub fn extract_mentions(content: &str) -> Vec<String> {
    let mut mentions = std::collections::HashSet::new();
    let mut chars = content.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '@' {
            let mut username = String::new();

            // Extract username: alphanumeric, hyphens, underscores
            while let Some(&next_ch) = chars.peek() {
                if next_ch.is_alphanumeric() || next_ch == '-' || next_ch == '_' {
                    username.push(next_ch);
                    chars.next();
                } else {
                    break;
                }
            }

            if !username.is_empty() {
                mentions.insert(username);
            }
        }
    }

    mentions.into_iter().collect()
}

/// Find users by their login names
pub async fn find_users_by_login_names(
    tx: &mut Transaction<'_, Postgres>,
    login_names: &[String],
) -> Result<Vec<(Uuid, String)>> {
    if login_names.is_empty() {
        return Ok(vec![]);
    }

    let users = sqlx::query!(
        r#"
        SELECT id, login_name
        FROM users
        WHERE login_name = ANY($1)
        "#,
        login_names
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(users.into_iter().map(|u| (u.id, u.login_name)).collect())
}

/// Soft-delete a comment by setting deleted_at and nulling out content
pub async fn delete_comment(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    reason: CommentDeletionReason,
) -> Result<()> {
    // Soft delete the comment
    sqlx::query!(
        r#"
        UPDATE comments
        SET
            deleted_at = now(),
            deletion_reason = $2,
            content = NULL,
            content_html = NULL
        WHERE id = $1
        "#,
        id,
        reason as CommentDeletionReason
    )
    .execute(&mut **tx)
    .await?;

    // Delete notifications referencing this comment
    sqlx::query!(
        r#"
        DELETE FROM notifications
        WHERE comment_id = $1
        "#,
        id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}
