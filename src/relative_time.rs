//! How long ago, in the few characters a post card has room for: "3h",
//! "2d", "3w". A card's caption shares one line between the artist, the
//! community and this, so it cannot spend a whole date on something whose
//! only question is "recent or not". Past a month it gives the date itself,
//! because "40w" answers nothing a reader is asking.

use chrono::{DateTime, Utc};
use chrono_tz::Asia::Seoul;

struct Units {
    now: &'static str,
    minute: &'static str,
    hour: &'static str,
    day: &'static str,
    week: &'static str,
}

fn units(lang: &str) -> Units {
    match lang.get(..2) {
        Some("ko") => Units {
            now: "방금",
            minute: "분",
            hour: "시간",
            day: "일",
            week: "주",
        },
        Some("ja") => Units {
            now: "たった今",
            minute: "分",
            hour: "時間",
            day: "日",
            week: "週",
        },
        Some("zh") => Units {
            now: "刚刚",
            minute: "分钟",
            hour: "小时",
            day: "天",
            week: "周",
        },
        _ => Units {
            now: "now",
            minute: "m",
            hour: "h",
            day: "d",
            week: "w",
        },
    }
}

/// `then` as time before `now`, in `lang`. A time in the future -- a clock
/// a little ahead of ours -- reads as "now" rather than as a negative.
pub fn ago(then: DateTime<Utc>, now: DateTime<Utc>, lang: &str) -> String {
    let u = units(lang);
    let minutes = (now - then).num_minutes();
    if minutes < 1 {
        return u.now.to_string();
    }
    if minutes < 60 {
        return format!("{minutes}{}", u.minute);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}{}", u.hour);
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}{}", u.day);
    }
    if days < 35 {
        return format!("{}{}", days / 7, u.week);
    }
    // The site's dates are Seoul's, as everywhere else it prints one.
    then.with_timezone(&Seoul).format("%Y-%m-%d").to_string()
}

/// The template filter: `{{ post.published_at|ago }}`, in the page's
/// language. A value that is not a timestamp comes back as it was.
pub fn ago_filter(state: &minijinja::State, value: String) -> String {
    let lang = state
        .lookup("ftl_lang")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "ko".to_string());
    match parse(&value) {
        Some(then) => ago(then, Utc::now(), &lang),
        None => value,
    }
}

/// A timestamp with its offset, or one without -- a `timestamp` column
/// comes out of chrono as a NaiveDateTime -- read as UTC, as the rest of
/// the site's templates read those.
fn parse(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(then) = DateTime::parse_from_rfc3339(value) {
        return Some(then.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 22, 12, 0, 0).unwrap()
    }

    #[test]
    fn each_range_in_its_unit() {
        let n = now();
        assert_eq!(ago(n - Duration::seconds(20), n, "en"), "now");
        assert_eq!(ago(n - Duration::minutes(5), n, "en"), "5m");
        assert_eq!(ago(n - Duration::hours(3), n, "en"), "3h");
        assert_eq!(ago(n - Duration::days(2), n, "en"), "2d");
        assert_eq!(ago(n - Duration::days(15), n, "en"), "2w");
    }

    #[test]
    fn past_a_month_it_is_the_date_in_seoul() {
        let n = now();
        // 20:00 UTC is already the next day in Seoul.
        let then = Utc.with_ymd_and_hms(2026, 3, 1, 20, 0, 0).unwrap();
        assert_eq!(ago(then, n, "en"), "2026-03-02");
    }

    #[test]
    fn in_the_pages_language() {
        let n = now();
        assert_eq!(ago(n - Duration::hours(3), n, "ko"), "3시간");
        assert_eq!(ago(n - Duration::days(2), n, "ja"), "2日");
        assert_eq!(ago(n - Duration::days(14), n, "zh-CN"), "2周");
    }

    #[test]
    fn a_timestamp_without_an_offset_is_utc() {
        assert_eq!(
            parse("2026-01-02T03:04:05"),
            Some(Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap())
        );
        assert_eq!(
            parse("2026-01-02T03:04:05.123456"),
            parse("2026-01-02T03:04:05.123456Z")
        );
        assert_eq!(parse("not a time"), None);
    }

    #[test]
    fn a_clock_ahead_of_ours_is_now() {
        let n = now();
        assert_eq!(ago(n + Duration::minutes(3), n, "en"), "now");
    }
}
