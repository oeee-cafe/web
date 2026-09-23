use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{query, query_as, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Follow {
    pub follower_actor_id: Uuid,
    pub following_actor_id: Uuid,
    pub created_at: DateTime<Utc>,
}

async fn get_actor_id_by_user_id(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Uuid> {
    let result = query!("SELECT id FROM actors WHERE user_id = $1", user_id)
        .fetch_one(&mut **tx)
        .await?;

    Ok(result.id)
}

pub async fn follow_user(
    tx: &mut Transaction<'_, Postgres>,
    follower_user_id: Uuid,
    following_user_id: Uuid,
) -> Result<Follow> {
    let follower_actor_id = get_actor_id_by_user_id(tx, follower_user_id).await?;
    let following_actor_id = get_actor_id_by_user_id(tx, following_user_id).await?;

    let query = query!(
        "INSERT INTO follows (follower_actor_id, following_actor_id) VALUES ($1, $2) RETURNING *",
        follower_actor_id,
        following_actor_id,
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(Follow {
        follower_actor_id: query.follower_actor_id,
        following_actor_id: query.following_actor_id,
        created_at: query.created_at,
    })
}

pub async fn unfollow_user(
    tx: &mut Transaction<'_, Postgres>,
    follower_user_id: Uuid,
    following_user_id: Uuid,
) -> Result<()> {
    let follower_actor_id = get_actor_id_by_user_id(tx, follower_user_id).await?;
    let following_actor_id = get_actor_id_by_user_id(tx, following_user_id).await?;

    query!(
        "DELETE FROM follows WHERE follower_actor_id = $1 AND following_actor_id = $2",
        follower_actor_id,
        following_actor_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

// Check if user is following another user
pub async fn is_following(
    tx: &mut Transaction<'_, Postgres>,
    follower_user_id: Uuid,
    following_user_id: Uuid,
) -> Result<bool> {
    let follower_actor_id = get_actor_id_by_user_id(tx, follower_user_id).await?;
    let following_actor_id = get_actor_id_by_user_id(tx, following_user_id).await?;

    let query = query!(
        "SELECT COUNT(*) FROM follows WHERE follower_actor_id = $1 AND following_actor_id = $2",
        follower_actor_id,
        following_actor_id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(query.count == Some(1))
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FollowingInfo {
    pub user_id: Uuid,
    pub login_name: String,
    pub display_name: String,
    pub banner_image_filename: Option<String>,
    pub banner_image_width: Option<i32>,
    pub banner_image_height: Option<i32>,
}

pub async fn find_followings_by_user_id(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    limit: i64,
    offset: i64,
    with_banners_only: bool,
) -> Result<Vec<FollowingInfo>> {
    let follower_actor_id = get_actor_id_by_user_id(tx, user_id).await?;

    let rows = query!(
        r#"SELECT
            actors.user_id,
            users.login_name,
            users.display_name,
            images.image_filename AS "image_filename?",
            images.width AS "width?",
            images.height AS "height?"
        FROM follows
        LEFT JOIN actors ON follows.following_actor_id = actors.id
        LEFT JOIN users ON actors.user_id = users.id
        LEFT JOIN banners ON users.banner_id = banners.id
        LEFT JOIN images ON banners.image_id = images.id
        WHERE follower_actor_id = $1
        ORDER BY users.display_name ASC
        LIMIT $2 OFFSET $3"#,
        follower_actor_id,
        limit,
        offset
    )
    .fetch_all(&mut **tx)
    .await?;

    let following_infos: Vec<FollowingInfo> = rows
        .into_iter()
        .filter_map(|row| {
            row.user_id.map(|user_id| FollowingInfo {
                user_id,
                login_name: row.login_name,
                display_name: row.display_name,
                banner_image_filename: row.image_filename,
                banner_image_width: row.width,
                banner_image_height: row.height,
            })
        })
        .filter(|f| {
            // Filter by banners in Rust if needed
            if with_banners_only {
                f.banner_image_filename.is_some()
            } else {
                true
            }
        })
        .collect();

    Ok(following_infos)
}

pub async fn create_follow_by_actor_ids(
    tx: &mut Transaction<'_, Postgres>,
    follower_actor_id: Uuid,
    following_actor_id: Uuid,
) -> Result<Follow> {
    let query = query!(
        "INSERT INTO follows (follower_actor_id, following_actor_id) VALUES ($1, $2) 
         ON CONFLICT (follower_actor_id, following_actor_id) DO NOTHING
         RETURNING *",
        follower_actor_id,
        following_actor_id,
    )
    .fetch_optional(&mut **tx)
    .await?;

    match query {
        Some(row) => Ok(Follow {
            follower_actor_id: row.follower_actor_id,
            following_actor_id: row.following_actor_id,
            created_at: row.created_at,
        }),
        None => {
            // Follow relationship already exists, fetch it
            let existing = query!(
                "SELECT * FROM follows WHERE follower_actor_id = $1 AND following_actor_id = $2",
                follower_actor_id,
                following_actor_id,
            )
            .fetch_one(&mut **tx)
            .await?;

            Ok(Follow {
                follower_actor_id: existing.follower_actor_id,
                following_actor_id: existing.following_actor_id,
                created_at: existing.created_at,
            })
        }
    }
}

pub async fn unfollow_by_actor_ids(
    tx: &mut Transaction<'_, Postgres>,
    follower_actor_id: Uuid,
    following_actor_id: Uuid,
) -> Result<()> {
    query!(
        "DELETE FROM follows WHERE follower_actor_id = $1 AND following_actor_id = $2",
        follower_actor_id,
        following_actor_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

use crate::models::actor::Actor;

pub async fn find_followers_by_actor_id(
    tx: &mut Transaction<'_, Postgres>,
    following_actor_id: Uuid,
) -> Result<Vec<Actor>> {
    let actors = query_as!(
        Actor,
        r#"
        SELECT 
            a.id, a.iri, a.type as "type: _", a.username, a.instance_host, 
            a.handle_host, a.handle, a.user_id, a.community_id, a.name, a.bio_html, 
            a.automatically_approves_followers, a.inbox_url, a.shared_inbox_url, 
            a.followers_url, a.sensitive, a.public_key_pem, a.private_key_pem, 
            a.url, a.created_at, a.updated_at, a.published_at
        FROM follows f
        JOIN actors a ON f.follower_actor_id = a.id
        WHERE f.following_actor_id = $1
        "#,
        following_actor_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(actors)
}

pub async fn get_follower_shared_inboxes_for_actor(
    tx: &mut Transaction<'_, Postgres>,
    following_actor_id: Uuid,
) -> Result<Vec<String>> {
    let inboxes = query!(
        r#"
        SELECT DISTINCT a.shared_inbox_url
        FROM follows f
        JOIN actors a ON f.follower_actor_id = a.id
        WHERE f.following_actor_id = $1
        "#,
        following_actor_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(inboxes
        .into_iter()
        .map(|row| row.shared_inbox_url)
        .collect())
}
