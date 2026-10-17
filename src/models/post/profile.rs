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
                posts.updated_at,
                EXISTS (SELECT 1 FROM pickles WHERE pickles.post_id = posts.id) AS \"pickled!\"
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
            pickled: row.pickled,
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

/// How many days in a row someone has drawn: the run they are on now, and
/// the longest they have managed. A day is Seoul's, as every date the site
/// prints is, and counts once it has one published drawing anyone can see
/// on the profile -- the public tab's drawings, so everyone who looks is
/// shown the same numbers and none are drawn from a private community.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct DrawingStreaks {
    /// The run that ends today or yesterday; a day not yet drawn in has
    /// not broken it. Nought once a whole day has gone by without one.
    pub current: i64,
    pub longest: i64,
}

pub async fn drawing_streaks(
    tx: &mut Transaction<'_, Postgres>,
    author_id: Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<DrawingStreaks> {
    let days = sqlx::query_scalar!(
        r#"
            SELECT DISTINCT (posts.published_at AT TIME ZONE 'Asia/Seoul')::date AS "day!"
            FROM posts
            JOIN images ON posts.image_id = images.id
            LEFT JOIN communities ON posts.community_id = communities.id
            WHERE posts.author_id = $1
            AND posts.published_at IS NOT NULL
            AND posts.deleted_at IS NULL
            AND (posts.community_id IS NULL OR communities.visibility = 'public')
            ORDER BY 1
        "#,
        author_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let today = now.with_timezone(&chrono_tz::Asia::Seoul).date_naive();
    Ok(streaks_from_days(&days, today))
}

/// The streaks in `days`, which are distinct and in order.
fn streaks_from_days(days: &[chrono::NaiveDate], today: chrono::NaiveDate) -> DrawingStreaks {
    let mut longest = 0;
    let mut run = 0;
    let mut previous: Option<chrono::NaiveDate> = None;
    for &day in days {
        run = match previous {
            Some(p) if p.succ_opt() == Some(day) => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        previous = Some(day);
    }
    let current = match previous {
        Some(last) if last == today || last.succ_opt() == Some(today) => run,
        _ => 0,
    };
    DrawingStreaks { current, longest }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
    }

    #[test]
    fn nobody_who_has_not_drawn_has_a_streak() {
        assert_eq!(streaks_from_days(&[], d(17)), DrawingStreaks::default());
    }

    #[test]
    fn a_run_lasts_until_a_whole_day_goes_by() {
        let days = [d(1), d(2), d(3), d(10), d(15), d(16)];
        // Drawn yesterday, not yet today: still on.
        assert_eq!(
            streaks_from_days(&days, d(17)),
            DrawingStreaks {
                current: 2,
                longest: 3
            }
        );
        assert_eq!(streaks_from_days(&days, d(16)).current, 2);
        // A day missed.
        assert_eq!(
            streaks_from_days(&days, d(18)),
            DrawingStreaks {
                current: 0,
                longest: 3
            }
        );
    }

    #[test]
    fn a_run_crosses_the_end_of_a_month() {
        let days = [
            NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
            d(1),
        ];
        assert_eq!(
            streaks_from_days(&days, d(1)),
            DrawingStreaks {
                current: 3,
                longest: 3
            }
        );
    }
}
