//! The overview's date range, which the user lists it links to carry along.
//!
//! Days are Seoul's, as every date the site prints is, and a range takes in
//! both of its ends whole.

use crate::app_error::AppError;
use crate::models::admin::AdminRange;
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Seoul;
use serde::Serialize;

const DATE_FORMAT: &str = "%Y-%m-%d";

/// The presets above the overview, as days back from today; `None` is all
/// time.
const PRESETS: [(&str, Option<i64>); 5] = [
    ("Today", Some(1)),
    ("7 days", Some(7)),
    ("30 days", Some(30)),
    ("90 days", Some(90)),
    ("All time", None),
];

/// From one day to another, both included. An open side runs to the
/// beginning or the end of time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateRange {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

/// A range as the templates use it.
#[derive(Debug, Serialize)]
pub struct DateRangeContext {
    pub from: String,
    pub to: String,
    /// `from=…&to=…`, for links that keep the range.
    pub query: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct DateRangePreset {
    pub label: &'static str,
    pub query: String,
    pub active: bool,
}

pub fn today() -> NaiveDate {
    Utc::now().with_timezone(&Seoul).date_naive()
}

impl DateRange {
    /// The range a query names, or `None` when it names none at all. A key
    /// sent empty is an open side, which is how "All time" is asked for:
    /// `from=&to=`.
    pub fn parse(from: Option<&str>, to: Option<&str>) -> Result<Option<Self>, AppError> {
        if from.is_none() && to.is_none() {
            return Ok(None);
        }
        let range = DateRange {
            from: parse_date(from)?,
            to: parse_date(to)?,
        };
        if let (Some(from), Some(to)) = (range.from, range.to)
            && from > to
        {
            return Err(AppError::BadRequest(format!(
                "The range starts on {from}, after it ends on {to}"
            )));
        }
        Ok(Some(range))
    }

    /// The `days` days up to and including `today`.
    pub fn last_days(today: NaiveDate, days: i64) -> Self {
        DateRange {
            from: Some(today - Duration::days(days - 1)),
            to: Some(today),
        }
    }

    /// From the start of `from` in Seoul to the start of the day after `to`.
    pub fn bounds(self) -> AdminRange {
        AdminRange {
            start: self.from.map(start_of),
            end: self.to.map(|to| start_of(to + Duration::days(1))),
        }
    }

    pub fn query(self) -> String {
        format!(
            "from={}&to={}",
            format_date(self.from),
            format_date(self.to)
        )
    }

    pub fn describe(self) -> String {
        match (self.from, self.to) {
            (None, None) => "all time".to_string(),
            (Some(from), Some(to)) if from == to => format!("{from}"),
            (Some(from), Some(to)) => format!("{from} – {to}"),
            (Some(from), None) => format!("since {from}"),
            (None, Some(to)) => format!("up to {to}"),
        }
    }

    pub fn context(self) -> DateRangeContext {
        DateRangeContext {
            from: format_date(self.from),
            to: format_date(self.to),
            query: self.query(),
            description: self.describe(),
        }
    }

    /// The presets, with the one this range is marked.
    pub fn presets(self, today: NaiveDate) -> Vec<DateRangePreset> {
        PRESETS
            .iter()
            .map(|&(label, days)| {
                let preset =
                    days.map_or_else(DateRange::default, |days| DateRange::last_days(today, days));
                DateRangePreset {
                    label,
                    query: preset.query(),
                    active: preset == self,
                }
            })
            .collect()
    }
}

fn parse_date(value: Option<&str>) -> Result<Option<NaiveDate>, AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => NaiveDate::parse_from_str(value, DATE_FORMAT)
            .map(Some)
            .map_err(|_| AppError::BadRequest(format!("Not a date: {value}"))),
    }
}

fn format_date(date: Option<NaiveDate>) -> String {
    date.map(|date| date.format(DATE_FORMAT).to_string())
        .unwrap_or_default()
}

/// Seoul has kept one offset since 1988, so a midnight there is never
/// skipped or doubled.
fn start_of(day: NaiveDate) -> chrono::DateTime<Utc> {
    Seoul
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight exists"))
        .earliest()
        .expect("Seoul's midnight exists")
        .with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, DATE_FORMAT).unwrap()
    }

    #[test]
    fn a_range_takes_in_both_its_days_in_seoul() {
        let range = DateRange {
            from: Some(day("2026-10-05")),
            to: Some(day("2026-10-11")),
        };
        let bounds = range.bounds();
        assert_eq!(
            bounds.start.unwrap().to_rfc3339(),
            "2026-10-04T15:00:00+00:00"
        );
        assert_eq!(
            bounds.end.unwrap().to_rfc3339(),
            "2026-10-11T15:00:00+00:00"
        );
    }

    #[test]
    fn nothing_asked_is_not_all_time() {
        assert_eq!(DateRange::parse(None, None).unwrap(), None);
        assert_eq!(
            DateRange::parse(Some(""), Some("")).unwrap(),
            Some(DateRange::default())
        );
        assert_eq!(DateRange::default().bounds(), AdminRange::default());
    }

    #[test]
    fn a_backwards_or_unreadable_range_is_refused() {
        assert!(DateRange::parse(Some("2026-10-11"), Some("2026-10-05")).is_err());
        assert!(DateRange::parse(Some("yesterday"), None).is_err());
    }

    #[test]
    fn the_preset_a_range_came_from_is_marked() {
        let today = day("2026-10-11");
        let presets = DateRange::last_days(today, 7).presets(today);
        let active: Vec<_> = presets
            .iter()
            .filter(|p| p.active)
            .map(|p| p.label)
            .collect();
        assert_eq!(active, ["7 days"]);
        assert_eq!(presets[1].query, "from=2026-10-05&to=2026-10-11");
        assert_eq!(presets[4].query, "from=&to=");
    }
}
