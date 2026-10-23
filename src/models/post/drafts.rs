//! Drawings on their way to being posts: drafts, and publishing them.

use anyhow::Result;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

use super::{PostDraft, SerializableDraftPost, SerializablePost, Tool};
use sqlx::postgres::types::PgInterval;

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

/// A draft that can be drawn on again: one of NEO's replays, recorded by this
/// site's own painter, which can open the file and carry on from where it
/// ends (`PainterHandle.resume`). Tegaki's replay has no frame to carry on
/// from, and a NEO applet's was not written by a painter that can read it
/// back.
#[derive(Debug, Clone)]
pub struct ContinuableDraft {
    pub post_id: Uuid,
    pub image_id: Uuid,
    pub community_id: Option<Uuid>,
    pub parent_post_id: Option<Uuid>,
    pub width: i32,
    pub height: i32,
    pub replay_filename: String,
    pub paint_duration_ms: i64,
}

/// Whether a drawing made with `tool` (as the database spells it) and saved
/// as `replay_filename` can be continued; see [`ContinuableDraft`].
pub fn is_continuable(tool: &str, replay_filename: Option<&str>) -> bool {
    matches!(tool, "neo-cucumber" | "cucumber")
        && replay_filename.is_some_and(|name| name.ends_with(".pch"))
}

/// `post_id`, if it is a draft of `author_id`'s that can be continued.
/// Published or deleted posts are not drafts any more, and somebody else's
/// draft is not theirs to draw on.
pub async fn find_continuable_draft(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    post_id: Uuid,
) -> Result<Option<ContinuableDraft>> {
    let row = query!(
        r#"
            SELECT
                posts.id,
                posts.image_id AS "image_id!",
                posts.community_id,
                posts.parent_post_id,
                images.width,
                images.height,
                images.replay_filename,
                images.tool::text AS "tool!",
                (EXTRACT(EPOCH FROM images.paint_duration) * 1000)::bigint AS "paint_duration_ms!"
            FROM posts
            JOIN images ON posts.image_id = images.id
            WHERE posts.id = $1
            AND posts.author_id = $2
            AND posts.published_at IS NULL
            AND posts.deleted_at IS NULL
        "#,
        post_id,
        author_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.and_then(|row| {
        if !is_continuable(&row.tool, row.replay_filename.as_deref()) {
            return None;
        }
        Some(ContinuableDraft {
            post_id: row.id,
            image_id: row.image_id,
            community_id: row.community_id,
            parent_post_id: row.parent_post_id,
            width: row.width,
            height: row.height,
            replay_filename: row.replay_filename?,
            paint_duration_ms: row.paint_duration_ms,
        })
    }))
}

/// What a continued draft is now: the drawing and its replay in place of
/// the ones it had, keeping the post -- its id, its community, where it
/// relays from. `paint_duration` and `stroke_count` are the whole drawing's,
/// as the painter counts them across every sitting, so an upload that
/// arrives twice writes the same row twice.
pub struct DraftReplacement {
    pub paint_duration: PgInterval,
    pub stroke_count: i32,
    pub width: i32,
    pub height: i32,
    pub image_filename: String,
    pub replay_filename: String,
    pub tool: Tool,
    pub client_draft_id: Option<Uuid>,
}

pub async fn replace_draft_drawing(
    tx: &mut Transaction<'_, Postgres>,
    draft: &ContinuableDraft,
    replacement: DraftReplacement,
) -> Result<()> {
    query!(
        r#"
            UPDATE images
            SET
                paint_duration = $1,
                stroke_count = $2,
                width = $3,
                height = $4,
                image_filename = $5,
                replay_filename = $6,
                tool = $7
            WHERE id = $8
        "#,
        replacement.paint_duration,
        replacement.stroke_count,
        replacement.width,
        replacement.height,
        replacement.image_filename,
        replacement.replay_filename,
        replacement.tool as _,
        draft.image_id
    )
    .execute(&mut **tx)
    .await?;
    // The browser's id for this save, so a second upload of it is answered
    // with this post (`find_post_id_by_client_draft_id`) and not drawn again.
    query!(
        r#"
            UPDATE posts
            SET
                updated_at = now(),
                client_draft_id = COALESCE($1, client_draft_id)
            WHERE id = $2
        "#,
        replacement.client_draft_id,
        draft.post_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
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
                images.replay_filename,
                images.tool::text AS \"tool!\",
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
            continuable: is_continuable(&row.tool, row.replay_filename.as_deref()),
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

#[cfg(test)]
mod continue_tests {
    use super::*;
    use chrono::Duration;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    async fn user(tx: &mut Transaction<'static, Postgres>, who: &str) -> Uuid {
        sqlx::query_scalar!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
            format!("{who}_{}", &Uuid::new_v4().simple().to_string()[..12])
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    fn draft(author_id: Uuid, tool: Tool, replay: &str) -> PostDraft {
        PostDraft {
            author_id,
            community_id: None,
            paint_duration: PgInterval::try_from(Duration::seconds(90)).unwrap(),
            stroke_count: 3,
            width: 300,
            height: 200,
            image_filename: format!("{}.png", Uuid::new_v4().simple()),
            replay_filename: Some(format!("{}.{replay}", Uuid::new_v4().simple())),
            tool,
            parent_post_id: None,
            client_draft_id: None,
        }
    }

    /// A draft is found for its author to carry on with, drawn on in place,
    /// and found by the id the browser gave the save that replaced it -- so
    /// a second upload of that save is answered with the same post.
    #[tokio::test]
    async fn a_draft_is_drawn_on_again_in_place() {
        let Some(mut tx) = tx().await else { return };
        let author = user(&mut tx, "artist").await;
        let stranger = user(&mut tx, "stranger").await;
        let post = create_post(&mut tx, draft(author, Tool::NeoCucumber, "pch"))
            .await
            .unwrap();

        assert!(find_continuable_draft(&mut tx, stranger, post.id)
            .await
            .unwrap()
            .is_none());
        let found = find_continuable_draft(&mut tx, author, post.id)
            .await
            .unwrap()
            .expect("the author's draft");
        assert_eq!((found.width, found.height), (300, 200));
        assert_eq!(found.paint_duration_ms, 90_000);
        assert!(found.replay_filename.ends_with(".pch"));

        let save = Uuid::new_v4();
        replace_draft_drawing(
            &mut tx,
            &found,
            DraftReplacement {
                paint_duration: PgInterval::try_from(Duration::seconds(150)).unwrap(),
                stroke_count: 7,
                width: 300,
                height: 200,
                image_filename: "continued.png".to_string(),
                replay_filename: "continued.pch".to_string(),
                tool: Tool::NeoCucumber,
                client_draft_id: Some(save),
            },
        )
        .await
        .unwrap();
        let again = find_continuable_draft(&mut tx, author, post.id)
            .await
            .unwrap()
            .expect("still a draft");
        assert_eq!(again.image_id, found.image_id);
        assert_eq!(again.replay_filename, "continued.pch");
        assert_eq!(again.paint_duration_ms, 150_000);
        assert_eq!(
            find_post_id_by_client_draft_id(&mut tx, author, save)
                .await
                .unwrap(),
            Some(post.id)
        );
        assert_eq!(
            find_draft_posts_by_author_id(&mut tx, author)
                .await
                .unwrap()
                .iter()
                .map(|draft| (draft.id, draft.continuable))
                .collect::<Vec<_>>(),
            vec![(post.id, true)]
        );
    }

    /// Tegaki's replay, a published drawing and a deleted one are not drafts
    /// to carry on with.
    #[tokio::test]
    async fn only_this_painters_unpublished_drafts_are_continued() {
        let Some(mut tx) = tx().await else { return };
        let author = user(&mut tx, "artist").await;
        let tegaki = create_post(&mut tx, draft(author, Tool::Tegaki, "tgkr"))
            .await
            .unwrap();
        let published = create_post(&mut tx, draft(author, Tool::NeoCucumber, "pch"))
            .await
            .unwrap();
        let deleted = create_post(&mut tx, draft(author, Tool::Cucumber, "pch"))
            .await
            .unwrap();
        sqlx::query!(
            "UPDATE posts SET published_at = now() WHERE id = $1",
            published.id
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE posts SET deleted_at = now() WHERE id = $1",
            deleted.id
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        for id in [tegaki.id, published.id, deleted.id] {
            assert!(find_continuable_draft(&mut tx, author, id)
                .await
                .unwrap()
                .is_none());
        }
        let drafts = find_draft_posts_by_author_id(&mut tx, author)
            .await
            .unwrap();
        assert_eq!(drafts.len(), 1);
        assert!(!drafts[0].continuable, "Tegaki's");
    }
}
