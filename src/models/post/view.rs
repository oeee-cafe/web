//! One post, and the thread of drawings under it.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Asia::Seoul;
use humantime::format_duration;
use sqlx::{query, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;

use super::SerializableThreadedPost;

type PostData = (
    Option<String>,        // title
    Option<String>,        // content
    Uuid,                  // author_id
    String,                // login_name
    String,                // display_name
    String,                // profile_image_url
    String,                // image_url
    i32,                   // comment_count
    i32,                   // like_count
    Option<DateTime<Utc>>, // published_at
    i64,                   // paint_duration_ms
    Option<String>,        // community_slug
);

pub async fn increment_post_viewer_count(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<()> {
    let q = query!(
        "
            UPDATE posts
            SET viewer_count = viewer_count + 1
            WHERE id = $1
        ",
        id
    );
    q.execute(&mut **tx).await?;
    Ok(())
}

pub async fn find_post_by_id(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<HashMap<String, Option<String>>>> {
    let q = query!(
        r#"
            SELECT
                posts.id,
                posts.title,
                posts.content,
                posts.is_sensitive,
                posts.author_id,
                images.id AS image_id,
                images.tool::text AS image_tool,
                images.paint_duration,
                images.width,
                images.height,
                images.image_filename,
                images.replay_filename,
                posts.viewer_count,
                posts.published_at,
                posts.created_at,
                posts.updated_at,
                posts.allow_relay,
                posts.allow_replay,
                posts.parent_post_id,
                users.display_name AS display_name,
                users.login_name AS login_name,
                communities.id AS "community_id?",
                communities.name AS "community_name?",
                communities.slug AS "community_slug?"
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            LEFT JOIN users ON posts.author_id = users.id
            WHERE posts.id = $1
            AND posts.deleted_at IS NULL
        "#,
        id
    );
    let result = q.fetch_optional(&mut **tx).await?;

    Ok(result.map(|row| {
        let mut map: HashMap<String, Option<String>> = HashMap::new();
        map.insert("id".to_string(), Some(row.id.to_string()));
        map.insert("author_id".to_string(), Some(row.author_id.to_string()));
        map.insert("display_name".to_string(), Some(row.display_name));
        map.insert("login_name".to_string(), Some(row.login_name));
        map.insert("title".to_string(), row.title);
        map.insert("content".to_string(), row.content);
        map.insert(
            "is_sensitive".to_string(),
            Some(row.is_sensitive.to_string()),
        );

        let paint_duration = Duration::try_seconds(row.paint_duration.microseconds / 1000000)
            .expect("Duration should be valid")
            .to_std()
            .expect("Duration should be convertible to std::time::Duration");
        let paint_duration_human_readable = format_duration(paint_duration);
        map.insert(
            "paint_duration".to_string(),
            Some(paint_duration_human_readable.to_string()),
        );

        map.insert("image_id".to_string(), Some(row.image_id.to_string()));
        map.insert(
            "image_tool".to_string(),
            Some(row.image_tool.unwrap_or_default()),
        );
        map.insert("image_width".to_string(), Some(row.width.to_string()));
        map.insert("image_height".to_string(), Some(row.height.to_string()));
        map.insert("image_filename".to_string(), Some(row.image_filename));
        map.insert("replay_filename".to_string(), row.replay_filename);
        map.insert(
            "viewer_count".to_string(),
            Some(row.viewer_count.to_string()),
        );
        map.insert("allow_relay".to_string(), Some(row.allow_relay.to_string()));
        map.insert(
            "allow_replay".to_string(),
            Some(row.allow_replay.to_string()),
        );

        let created_at_seoul = row.created_at.with_timezone(&Seoul);
        let created_at_human_readable = created_at_seoul.format("%Y-%m-%d %H:%M").to_string();
        map.insert("created_at".to_string(), Some(created_at_human_readable));

        match row.published_at {
            None => {
                map.insert("published_at".to_string(), None);
            }
            Some(published_at) => {
                let published_at_seoul = published_at.with_timezone(&Seoul);
                let published_at_human_readable =
                    published_at_seoul.format("%Y-%m-%d %H:%M").to_string();
                map.insert(
                    "published_at".to_string(),
                    Some(published_at_human_readable),
                );

                // Insert UTC published_at time with timezone
                map.insert(
                    "published_at_utc".to_string(),
                    Some(published_at.to_rfc3339()),
                );
            }
        }

        let updated_at_seoul = row.updated_at.with_timezone(&Seoul);
        let updated_at_human_readable = updated_at_seoul.format("%Y-%m-%d %H:%M").to_string();
        map.insert("updated_at".to_string(), Some(updated_at_human_readable));

        // Insert UTC updated_at time with timezone
        map.insert(
            "updated_at_utc".to_string(),
            Some(row.updated_at.to_rfc3339()),
        );
        map.insert(
            "community_id".to_string(),
            row.community_id.map(|id| id.to_string()),
        );
        map.insert("community_name".to_string(), row.community_name.clone());
        map.insert("community_slug".to_string(), row.community_slug.clone());
        map.insert(
            "parent_post_id".to_string(),
            row.parent_post_id.map(|id| id.to_string()),
        );
        map
    }))
}

pub async fn find_child_posts_by_parent_id(
    tx: &mut Transaction<'_, Postgres>,
    parent_post_id: Uuid,
) -> Result<Vec<SerializableThreadedPost>> {
    let q = query!(
        "
            SELECT
                posts.id,
                posts.title,
                posts.content,
                posts.author_id,
                users.login_name,
                users.display_name,
                actors.handle as actor_handle,
                images.image_filename,
                images.width,
                images.height,
                posts.published_at,
                communities.slug AS \"community_slug?\"
            FROM posts
            LEFT JOIN images ON posts.image_id = images.id
            LEFT JOIN users ON posts.author_id = users.id
            LEFT JOIN actors ON actors.user_id = users.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.parent_post_id = $1
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            ORDER BY posts.published_at ASC
        ",
        parent_post_id
    );
    let result = q.fetch_all(&mut **tx).await?;

    Ok(result
        .into_iter()
        .map(|row| {
            // Format the published_at date
            let published_at_formatted = row.published_at.as_ref().map(|dt| {
                use chrono::TimeZone;
                let seoul = chrono_tz::Asia::Seoul;
                let seoul_time = seoul.from_utc_datetime(&dt.naive_utc());
                seoul_time.format("%Y-%m-%d %H:%M").to_string()
            });

            SerializableThreadedPost {
                id: row.id,
                title: row.title,
                content: row.content,
                author_id: row.author_id,
                user_login_name: row.login_name.into(),
                user_display_name: row.display_name,
                user_actor_handle: row.actor_handle,
                image_filename: row.image_filename,
                image_width: row.width,
                image_height: row.height,
                published_at: row.published_at,
                published_at_formatted,
                comments_count: 0, // Will be populated by build_thread_tree
                community_slug: row.community_slug,
                children: Vec::new(), // Will be populated by build_thread_tree
            }
        })
        .collect())
}

pub async fn build_thread_tree(
    tx: &mut Transaction<'_, Postgres>,
    parent_post_id: Uuid,
) -> Result<Vec<SerializableThreadedPost>> {
    use std::collections::HashMap;

    // Use recursive CTE to fetch all descendants in a single query
    let rows = query!(
        r#"
            WITH RECURSIVE post_tree AS (
                -- Base case: direct children
                SELECT
                    posts.id,
                    posts.title,
                    posts.content,
                    posts.author_id,
                    posts.parent_post_id,
                    users.login_name,
                    users.display_name,
                    actors.handle as actor_handle,
                    images.image_filename,
                    images.width,
                    images.height,
                    posts.published_at,
                    COALESCE(comment_counts.count, 0) as comments_count,
                    communities.slug as community_slug
                FROM posts
                LEFT JOIN images ON posts.image_id = images.id
                LEFT JOIN users ON posts.author_id = users.id
                LEFT JOIN actors ON actors.user_id = users.id
                LEFT JOIN communities ON posts.community_id = communities.id
                LEFT JOIN (
                    SELECT post_id, COUNT(*) as count
                    FROM comments
                    GROUP BY post_id
                ) comment_counts ON posts.id = comment_counts.post_id
                WHERE posts.parent_post_id = $1
                AND posts.published_at IS NOT NULL
                AND posts.deleted_at IS NULL

                UNION ALL

                -- Recursive case: children of children
                SELECT
                    p.id,
                    p.title,
                    p.content,
                    p.author_id,
                    p.parent_post_id,
                    u.login_name,
                    u.display_name,
                    a.handle as actor_handle,
                    i.image_filename,
                    i.width,
                    i.height,
                    p.published_at,
                    COALESCE(cc.count, 0) as comments_count,
                    c.slug as community_slug
                FROM posts p
                LEFT JOIN images i ON p.image_id = i.id
                LEFT JOIN users u ON p.author_id = u.id
                LEFT JOIN actors a ON a.user_id = u.id
                LEFT JOIN communities c ON p.community_id = c.id
                LEFT JOIN (
                    SELECT post_id, COUNT(*) as count
                    FROM comments
                    GROUP BY post_id
                ) cc ON p.id = cc.post_id
                INNER JOIN post_tree pt ON p.parent_post_id = pt.id
                WHERE p.published_at IS NOT NULL
                AND p.deleted_at IS NULL
            )
            SELECT * FROM post_tree
            ORDER BY published_at ASC
        "#,
        parent_post_id
    )
    .fetch_all(&mut **tx)
    .await?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Build a map to track children for each parent
    let mut children_map: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    let mut post_data: HashMap<Uuid, PostData> = HashMap::new();

    for row in &rows {
        // Skip posts with missing required data
        let Some(id) = row.id else { continue };
        let Some(author_id) = row.author_id else {
            continue;
        };
        let Some(login_name) = &row.login_name else {
            continue;
        };
        let Some(display_name) = &row.display_name else {
            continue;
        };
        let Some(actor_handle) = &row.actor_handle else {
            continue;
        };
        let Some(image_filename) = &row.image_filename else {
            continue;
        };
        let Some(width) = row.width else { continue };
        let Some(height) = row.height else { continue };
        let Some(comments_count) = row.comments_count else {
            continue;
        };

        post_data.insert(
            id,
            (
                row.title.clone(),
                row.content.clone(),
                author_id,
                login_name.clone(),
                display_name.clone(),
                actor_handle.clone(),
                image_filename.clone(),
                width,
                height,
                row.published_at,
                comments_count,
                row.community_slug.clone(),
            ),
        );

        if let Some(parent_id) = row.parent_post_id {
            children_map.entry(parent_id).or_default().push(id);
        }
    }

    // Recursive function to build tree for a given post ID
    fn build_subtree(
        post_id: Uuid,
        post_data: &HashMap<Uuid, PostData>,
        children_map: &HashMap<Uuid, Vec<Uuid>>,
    ) -> Option<SerializableThreadedPost> {
        let (
            title,
            content,
            author_id,
            login_name,
            display_name,
            actor_handle,
            image_filename,
            width,
            height,
            published_at,
            comments_count,
            community_slug,
        ) = post_data.get(&post_id)?;

        // Format the published_at date
        let published_at_formatted = published_at.as_ref().map(|dt| {
            use chrono::TimeZone;
            let seoul = chrono_tz::Asia::Seoul;
            let seoul_time = seoul.from_utc_datetime(&dt.naive_utc());
            seoul_time.format("%Y-%m-%d %H:%M").to_string()
        });

        let children = children_map
            .get(&post_id)
            .map(|child_ids| {
                child_ids
                    .iter()
                    .filter_map(|child_id| build_subtree(*child_id, post_data, children_map))
                    .collect()
            })
            .unwrap_or_default();

        Some(SerializableThreadedPost {
            id: post_id,
            title: title.clone(),
            content: content.clone(),
            author_id: *author_id,
            user_login_name: (login_name.clone()).into(),
            user_display_name: display_name.clone(),
            user_actor_handle: actor_handle.clone(),
            image_filename: image_filename.clone(),
            image_width: *width,
            image_height: *height,
            published_at: *published_at,
            published_at_formatted,
            comments_count: *comments_count,
            community_slug: community_slug.clone(),
            children,
        })
    }

    // Build trees for all root posts (direct children of parent_post_id)
    let result: Vec<SerializableThreadedPost> = rows
        .iter()
        .filter_map(|row| {
            let id = row.id?;
            if row.parent_post_id == Some(parent_post_id) {
                build_subtree(id, &post_data, &children_map)
            } else {
                None
            }
        })
        .collect();

    Ok(result)
}
