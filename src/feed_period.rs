//! Which month a drawing in a feed belongs to, for the headings that break
//! a grid of drawings up.
//!
//! A month and not a day or a week, because of how many drawings there are:
//! a few a week. Headed by day, or by today, this week and last week, the
//! top of Home was a heading over one drawing, then another over one more
//! -- rows of a single square with the rest of the row empty, in the part
//! of the page most people see. A month holds enough to fill its rows.
//! Each card already says how long ago it was drawn.
//!
//! Months run in order, so a feed that is newest first meets each once,
//! and a heading is only ever the first drawing of a new one. They are
//! Seoul's, as every date the site prints is.

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Asia::Seoul;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Period {
    /// The same for every drawing in the month, "2026-08", and what a
    /// feed's next batch is told the last one ended in, so it does not head
    /// its first drawing with the heading already above it.
    pub key: String,
    pub month: u32,
    /// Only for a month of another year than this one.
    pub year: Option<String>,
}

/// The month `then` falls in, seen from `now`.
pub fn period(then: DateTime<Utc>, now: DateTime<Utc>) -> Period {
    let this_year = now.with_timezone(&Seoul).year();
    let day = then.with_timezone(&Seoul).date_naive();
    Period {
        key: format!("{:04}-{:02}", day.year(), day.month()),
        month: day.month(),
        year: (day.year() != this_year).then(|| day.year().to_string()),
    }
}

/// The heading each drawing opens, if it opens one: the first of every
/// month, except a first drawing still in `after` -- the month the
/// previous batch ended in.
pub fn headings(
    times: impl IntoIterator<Item = Option<DateTime<Utc>>>,
    after: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<Option<Period>> {
    let mut current = after.map(str::to_string);
    times
        .into_iter()
        .map(|then| {
            // A drawing with no date (none of the feeds have one) sits in
            // whatever month it is between.
            let period = period(then?, now);
            if current.as_deref() == Some(period.key.as_str()) {
                return None;
            }
            current = Some(period.key.clone());
            Some(period)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 1, 0, 0).unwrap()
    }

    /// Noon in Seoul on the given day.
    fn seoul(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 3, 0, 0).unwrap()
    }

    /// The month turns in Seoul, not in UTC: 16:00 UTC on 31 August is
    /// 01:00 on 1 September there.
    #[test]
    fn months_are_seouls() {
        let early = Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap();
        assert_eq!(period(early, now()).key, "2026-09");
        let late = Utc.with_ymd_and_hms(2026, 8, 31, 14, 59, 0).unwrap();
        assert_eq!(period(late, now()).key, "2026-08");
    }

    /// What counts as this year is Seoul's too.
    #[test]
    fn a_month_names_its_year_only_when_it_is_not_this_one() {
        let august = period(seoul(2026, 8, 1), now());
        assert_eq!((august.month, august.year), (8, None));
        let last_december = period(seoul(2025, 12, 1), now());
        assert_eq!(last_december.key, "2025-12");
        assert_eq!(last_december.year.as_deref(), Some("2025"));
        let new_years_eve_in_utc = Utc.with_ymd_and_hms(2026, 12, 31, 16, 0, 0).unwrap();
        assert_eq!(
            period(seoul(2026, 12, 1), new_years_eve_in_utc)
                .year
                .as_deref(),
            Some("2026"),
            "already 2027 in Seoul"
        );
    }

    #[test]
    fn only_the_first_drawing_of_a_month_opens_a_heading() {
        let times = [
            seoul(2026, 9, 24),
            seoul(2026, 9, 5),
            seoul(2026, 8, 30),
            seoul(2026, 8, 3),
            seoul(2025, 12, 1),
        ];
        let keys: Vec<Option<String>> = headings(times.map(Some), None, now())
            .into_iter()
            .map(|h| h.map(|p| p.key))
            .collect();
        assert_eq!(
            keys,
            [
                Some("2026-09".to_string()),
                None,
                Some("2026-08".to_string()),
                None,
                Some("2025-12".to_string()),
            ]
        );
    }

    /// A batch that carries on the month the last one ended in does not
    /// repeat its heading, and one that starts a new month opens it.
    #[test]
    fn a_batch_continues_the_month_before_it() {
        let batch = [Some(seoul(2026, 8, 20)), Some(seoul(2026, 7, 30))];
        let carried = headings(batch, Some("2026-08"), now());
        assert_eq!(carried[0], None);
        assert_eq!(carried[1].as_ref().map(|p| p.key.as_str()), Some("2026-07"));
        let fresh = headings(batch, Some("2026-09"), now());
        assert_eq!(fresh[0].as_ref().map(|p| p.key.as_str()), Some("2026-08"));
    }
}
