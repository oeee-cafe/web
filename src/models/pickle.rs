//! Pickles: a thank-you rarer than a reaction. A supporter has one a week to
//! give to somebody else's post, and one not given by the end of the week is
//! gone (20261017090000_add_pickles says why nothing is stored until then).
//!
//! Every pickle given records its source. Only [`Source::Weekly`] exists now;
//! pickles bought on their own would be another, paid from a purchase, and
//! would never be confused with the weekly one.

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{query, query_scalar, Postgres, Transaction};
use uuid::Uuid;

/// Where a given pickle came from: `pickles.source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A supporter's pickle for the week.
    Weekly,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Weekly => "weekly",
        }
    }
}

/// The Monday, in Seoul, of the week `at` falls in: the `week` a weekly
/// pickle given then is recorded under. Seoul's for the reason
/// `supporter::current_year` gives.
pub fn week_of(at: DateTime<Utc>) -> NaiveDate {
    let today = at.with_timezone(&chrono_tz::Asia::Seoul).date_naive();
    today - Duration::days(i64::from(today.weekday().num_days_from_monday()))
}

/// What a viewer sees of a post's pickles, and what they may do.
#[derive(Clone, Debug, Serialize)]
pub struct PickleState {
    /// How many it has been given, from every source alike.
    pub count: i64,
    /// Whether the viewer has given it one.
    pub given_by_viewer: bool,
    /// Whether the viewer has this week's pickle still to give. False for
    /// anyone who is not a supporter this year.
    pub weekly_available: bool,
}

/// Whether `user_id` has given this week's pickle already.
pub async fn weekly_given(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<bool> {
    let given = query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM pickles
            WHERE giver_id = $1 AND source = 'weekly' AND week = $2
        ) AS "given!"
        "#,
        user_id,
        week_of(Utc::now()),
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(given)
}

/// Gives `post_id` this week's pickle from `giver_id`. False, and nothing
/// written, when they have given this week's already or have given this post
/// one before. Whether they are a supporter, and whether the post is one
/// they may give to, is the caller's to have checked.
pub async fn give_weekly(
    tx: &mut Transaction<'_, Postgres>,
    giver_id: Uuid,
    post_id: Uuid,
) -> Result<bool> {
    let result = query!(
        r#"
        INSERT INTO pickles (giver_id, post_id, source, week)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        "#,
        giver_id,
        post_id,
        Source::Weekly.as_str(),
        week_of(Utc::now()),
    )
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `post_id`'s pickles as `viewer_id` sees them. `is_supporter` is whether
/// the viewer is one this year (`supporter::mark_for`), which decides
/// whether they have a weekly pickle at all.
pub async fn state_for(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    viewer_id: Option<Uuid>,
    is_supporter: bool,
) -> Result<PickleState> {
    let row = query!(
        r#"
        SELECT
            count(*) AS "count!",
            COALESCE(bool_or(giver_id = $2), false) AS "given_by_viewer!"
        FROM pickles
        WHERE post_id = $1
        "#,
        post_id,
        viewer_id,
    )
    .fetch_one(&mut **tx)
    .await?;
    let weekly_available = match viewer_id {
        Some(viewer_id) if is_supporter => !weekly_given(tx, viewer_id).await?,
        _ => false,
    };
    Ok(PickleState {
        count: row.count,
        given_by_viewer: row.given_by_viewer,
        weekly_available,
    })
}

/// How many pickles `user_id`'s drawings have been given, from every source
/// alike: the number on their profile. A drawing they deleted takes its
/// pickles with it.
pub async fn received_by(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<i64> {
    let count = query_scalar!(
        r#"
        SELECT count(*) AS "count!"
        FROM pickles
        JOIN posts ON posts.id = pickles.post_id
        WHERE posts.author_id = $1 AND posts.deleted_at IS NULL
        "#,
        user_id,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn monday(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_week_begins_on_monday_in_seoul() {
        // Sunday 2026-10-18 23:59 in Seoul is still the week of the 12th.
        let sunday_night = Utc.with_ymd_and_hms(2026, 10, 18, 14, 59, 0).unwrap();
        assert_eq!(week_of(sunday_night), monday(2026, 10, 12));
        // A minute later it is Monday in Seoul, and Sunday still in UTC.
        let monday_morning = Utc.with_ymd_and_hms(2026, 10, 18, 15, 0, 0).unwrap();
        assert_eq!(week_of(monday_morning), monday(2026, 10, 19));
    }

    #[test]
    fn a_week_can_straddle_a_year() {
        // Thursday 2026-01-01 is in the week that began Monday 2025-12-29.
        let new_year = Utc.with_ymd_and_hms(2026, 1, 1, 3, 0, 0).unwrap();
        assert_eq!(week_of(new_year), monday(2025, 12, 29));
    }
}
