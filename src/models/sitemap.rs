use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{query, Postgres, Transaction};

/// One `<url>` entry: a path relative to the site root plus its last
/// modification time.
pub struct SitemapEntry {
    pub path: String,
    pub last_modified: DateTime<Utc>,
}

/// Published posts that a signed-out visitor can actually reach — the same
/// visibility rule the public feeds use: no community, or a public one.
pub async fn sitemap_posts(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<SitemapEntry>> {
    let rows = query!(
        r#"
            SELECT
                posts.id,
                -- A post in a community lives at /@community-slug/id and only
                -- redirects from the author's handle, so the sitemap has to
                -- name the same handle the post page calls itself by.
                COALESCE(NULLIF(communities.slug, ''), users.login_name) AS "handle!",
                posts.updated_at
            FROM posts
            JOIN users ON posts.author_id = users.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND users.deleted_at IS NULL
            AND (communities.visibility = 'public' OR posts.community_id IS NULL)
            ORDER BY posts.published_at DESC
            LIMIT $1
        "#,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| SitemapEntry {
            path: format!("/@{}/{}", row.handle, row.id),
            last_modified: row.updated_at,
        })
        .collect())
}

/// Profiles of users who have at least one publicly visible post. Listing every
/// account would fill the sitemap with empty pages.
pub async fn sitemap_profiles(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<SitemapEntry>> {
    let rows = query!(
        r#"
            SELECT users.login_name, MAX(posts.published_at) AS last_published
            FROM users
            JOIN posts ON posts.author_id = users.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE users.deleted_at IS NULL
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND (communities.visibility = 'public' OR posts.community_id IS NULL)
            GROUP BY users.login_name
            ORDER BY MAX(posts.published_at) DESC
            LIMIT $1
        "#,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.last_published.map(|last_published| SitemapEntry {
                path: format!("/@{}", row.login_name),
                last_modified: last_published,
            })
        })
        .collect())
}

pub async fn sitemap_communities(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<SitemapEntry>> {
    let rows = query!(
        r#"
            SELECT slug, updated_at
            FROM communities
            WHERE visibility = 'public'
            AND deleted_at IS NULL
            AND slug <> ''
            ORDER BY updated_at DESC
            LIMIT $1
        "#,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| SitemapEntry {
            path: format!("/@{}", row.slug),
            last_modified: row.updated_at,
        })
        .collect())
}

/// Tags that have at least one publicly visible post, newest activity first.
///
/// `hashtag_stats` is the same view the tag pages count from, so the sitemap
/// cannot advertise a tag whose page would come back empty.
pub async fn sitemap_hashtags(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<SitemapEntry>> {
    let rows = query!(
        r#"
            SELECT h.name, s.last_posted_at
            FROM hashtags h
            JOIN hashtag_stats s ON s.hashtag_id = h.id
            ORDER BY s.last_posted_at DESC
            LIMIT $1
        "#,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.last_posted_at.map(|last_posted_at| SitemapEntry {
                path: format!("/hashtags/{}", urlencoding::encode(&row.name)),
                last_modified: last_posted_at,
            })
        })
        .collect())
}
