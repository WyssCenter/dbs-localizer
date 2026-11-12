// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Date helpers for the record's three formats (`specs/16` §3): `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, RFC 3339 `Z`.

use jiff::civil::{Date, DateTime};
use jiff::{Span, Timestamp};

pub fn parse_local_date(value: &str) -> Option<Date> {
    if value.len() != 10 {
        return None;
    }
    value.parse::<Date>().ok()
}

pub fn parse_local_datetime(value: &str) -> Option<DateTime> {
    if value.len() != 16 || value.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    value.parse::<DateTime>().ok()
}

pub fn parse_instant(value: &str) -> Option<Timestamp> {
    if !value.ends_with('Z') || value.len() < 17 {
        return None;
    }
    value.parse::<Timestamp>().ok()
}

fn recognised_date(value: &str) -> Option<Date> {
    if let Some(d) = parse_local_date(value) {
        return Some(d);
    }
    if let Some(dt) = parse_local_datetime(value) {
        return Some(dt.date());
    }
    parse_instant(value).map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date())
}

pub fn shift_days(value: &str, days: i32) -> Option<String> {
    let date = recognised_date(value)?;
    let shifted = date
        .checked_add(Span::new().try_days(i64::from(days)).ok()?)
        .ok()?;
    let head = format!(
        "{:04}-{:02}-{:02}",
        shifted.year(),
        shifted.month(),
        shifted.day()
    );
    Some(format!("{head}{}", value.get(10..)?))
}

pub fn age_years(dob: &str, at: &str) -> Option<u32> {
    let born = recognised_date(dob)?;
    let then = recognised_date(at)?;
    if then < born {
        return None;
    }
    let mut years = i32::from(then.year()) - i32::from(born.year());
    if (then.month(), then.day()) < (born.month(), born.day()) {
        years -= 1;
    }
    u32::try_from(years).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_across_leap_day() {
        assert_eq!(shift_days("2028-02-28", 1).as_deref(), Some("2028-02-29"));
        assert_eq!(shift_days("2028-02-29", 1).as_deref(), Some("2028-03-01"));
        assert_eq!(shift_days("2028-03-01", -1).as_deref(), Some("2028-02-29"));
        assert_eq!(shift_days("2027-02-28", 1).as_deref(), Some("2027-03-01"));
        assert_eq!(shift_days("2028-02-10", 30).as_deref(), Some("2028-03-11"));
    }

    #[test]
    fn shifts_across_month_and_year() {
        assert_eq!(shift_days("2026-12-25", 10).as_deref(), Some("2027-01-04"));
        assert_eq!(shift_days("2026-01-05", -10).as_deref(), Some("2025-12-26"));
        assert_eq!(shift_days("2026-03-31", 1).as_deref(), Some("2026-04-01"));
        assert_eq!(shift_days("2026-03-31", 0).as_deref(), Some("2026-03-31"));
    }

    #[test]
    fn keeps_time_of_day_in_all_formats() {
        assert_eq!(
            shift_days("2026-09-30T08:51", 5).as_deref(),
            Some("2026-10-05T08:51")
        );
        assert_eq!(
            shift_days("2026-10-23T23:59:30Z", 1).as_deref(),
            Some("2026-10-24T23:59:30Z")
        );
        assert_eq!(
            shift_days("2026-10-23T00:00:00Z", -30).as_deref(),
            Some("2026-09-23T00:00:00Z")
        );
    }

    #[test]
    fn rejects_other_strings() {
        for bad in [
            "",
            "Walking",
            "2026-13-01",
            "2026-02-30",
            "2026-9-30",
            "2026-09-30 08:51",
            "2026-09-30T08:51:00",
            "11:00",
            "2026-09-30T08:51+01:00",
            "x2026-09-30",
        ] {
            assert_eq!(shift_days(bad, 3), None, "{bad}");
        }
    }

    #[test]
    fn age_is_exact() {
        assert_eq!(age_years("1959-06-25", "2026-09-30T08:51"), Some(67));
        assert_eq!(age_years("1959-06-25", "2026-06-24"), Some(66));
        assert_eq!(age_years("1959-06-25", "2026-06-25"), Some(67));
        assert_eq!(age_years("2026-06-25", "1959-06-25"), None);
        assert_eq!(age_years("bad", "2026-06-25"), None);
    }

    #[test]
    fn parsers_are_strict() {
        assert!(parse_local_date("2026-09-30").is_some());
        assert!(parse_local_datetime("2026-09-30T08:51").is_some());
        assert!(parse_instant("2026-10-23T12:00:00Z").is_some());
        assert!(parse_local_date("2026-09-30T08:51").is_none());
        assert!(parse_instant("2026-10-23T12:00:00").is_none());
    }
}
