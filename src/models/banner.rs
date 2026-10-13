use crate::models::post::Tool;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{postgres::types::PgInterval, query, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Banner {
    pub id: Uuid,
    pub image_id: Uuid,
    pub author_id: Uuid,
    pub created_at: DateTime<Utc>,
}

pub struct BannerDraft {
    pub author_id: Uuid,
    pub paint_duration: PgInterval,
    pub stroke_count: i32,
    pub width: i32,
    pub height: i32,
    pub image_filename: String,
    pub replay_filename: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SerializableBanner {
    pub id: Uuid,
    pub author_id: Uuid,
    pub paint_duration: String,
    pub stroke_count: i32,
    pub width: i32,
    pub height: i32,
    pub image_filename: String,
    pub replay_filename: Option<String>,
    pub viewer_count: i32,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub async fn create_banner(
    tx: &mut Transaction<'_, Postgres>,
    banner_draft: BannerDraft,
) -> Result<SerializableBanner> {
    let image = query!(
        "
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
        ",
        banner_draft.paint_duration,
        banner_draft.stroke_count,
        banner_draft.width,
        banner_draft.height,
        banner_draft.image_filename,
        banner_draft.replay_filename,
        Tool::NeoCucumber as _
    )
    .fetch_one(&mut **tx)
    .await?;

    let post = query!(
        "
            INSERT INTO banners (
                author_id,
                image_id
            )
            VALUES ($1, $2)
            RETURNING id, created_at
        ",
        banner_draft.author_id,
        image.id,
    )
    .fetch_one(&mut **tx)
    .await?;

    query!(
        "
            UPDATE users
            SET banner_id = $1
            WHERE id = $2
        ",
        post.id,
        banner_draft.author_id,
    )
    .execute(&mut **tx)
    .await?;

    Ok(SerializableBanner {
        id: post.id,
        author_id: banner_draft.author_id,
        paint_duration: banner_draft.paint_duration.microseconds.to_string(),
        stroke_count: banner_draft.stroke_count,
        image_filename: banner_draft.image_filename,
        replay_filename: banner_draft.replay_filename,
        width: banner_draft.width,
        height: banner_draft.height,
        viewer_count: 0,
        published_at: None,
        created_at: post.created_at,
    })
}

pub async fn find_banner_by_id(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<SerializableBanner> {
    let banner = query!(
        "
            SELECT
                banners.id,
                banners.author_id,
                images.paint_duration,
                images.stroke_count,
                images.width,
                images.height,
                images.image_filename,
                images.replay_filename,
                banners.created_at
            FROM banners
            JOIN images ON banners.image_id = images.id
            WHERE banners.id = $1
            AND banners.deleted_at IS NULL
        ",
        id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(SerializableBanner {
        id: banner.id,
        author_id: banner.author_id,
        paint_duration: banner.paint_duration.microseconds.to_string(),
        stroke_count: banner.stroke_count,
        image_filename: banner.image_filename,
        replay_filename: banner.replay_filename,
        width: banner.width,
        height: banner.height,
        viewer_count: 0,
        published_at: None,
        created_at: banner.created_at,
    })
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct BannerListItem {
    pub id: Uuid,
    pub image_filename: String,
    pub created_at: DateTime<Utc>,
    pub is_active: bool,
}

pub async fn list_user_banners(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<BannerListItem>> {
    let banners = query!(
        "
            SELECT
                banners.id,
                images.image_filename,
                banners.created_at,
                users.banner_id
            FROM banners
            LEFT JOIN images ON banners.image_id = images.id
            LEFT JOIN users ON users.id = banners.author_id
            WHERE banners.author_id = $1
            AND banners.deleted_at IS NULL
            ORDER BY banners.created_at DESC
        ",
        user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(banners
        .into_iter()
        .map(|banner| BannerListItem {
            id: banner.id,
            image_filename: banner.image_filename,
            created_at: banner.created_at,
            is_active: banner.banner_id == Some(banner.id),
        })
        .collect())
}

pub async fn activate_banner(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    banner_id: Uuid,
) -> Result<()> {
    // Verify the banner belongs to the user and is not deleted
    let banner = query!(
        "
            SELECT author_id
            FROM banners
            WHERE id = $1
            AND deleted_at IS NULL
        ",
        banner_id
    )
    .fetch_one(&mut **tx)
    .await?;

    if banner.author_id != user_id {
        anyhow::bail!("Banner does not belong to user");
    }

    // Update user's banner_id
    query!(
        "
            UPDATE users
            SET banner_id = $1
            WHERE id = $2
        ",
        banner_id,
        user_id,
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub async fn delete_banner(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    banner_id: Uuid,
) -> Result<()> {
    // Verify the banner belongs to the user and get image_id
    let banner = query!(
        "
            SELECT author_id, image_id
            FROM banners
            WHERE id = $1
            AND deleted_at IS NULL
        ",
        banner_id
    )
    .fetch_one(&mut **tx)
    .await?;

    if banner.author_id != user_id {
        anyhow::bail!("Banner does not belong to user");
    }

    // Check if this is the active banner
    let user = query!(
        "
            SELECT banner_id
            FROM users
            WHERE id = $1
        ",
        user_id
    )
    .fetch_one(&mut **tx)
    .await?;

    if user.banner_id == Some(banner_id) {
        anyhow::bail!("Cannot delete active banner");
    }

    // Soft delete the banner
    query!(
        "
            UPDATE banners
            SET deleted_at = now()
            WHERE id = $1
        ",
        banner_id
    )
    .execute(&mut **tx)
    .await?;

    // Soft delete the associated image
    query!(
        "
            UPDATE images
            SET deleted_at = now()
            WHERE id = $1
        ",
        banner.image_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}
