// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal UTC timestamp helpers (no `chrono` dependency).

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix epoch milliseconds, saturating at 0 before the epoch.
pub fn now_unix_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// RFC 3339 UTC (`2026-10-10T12:34:56Z`) from Unix epoch milliseconds.
pub fn rfc3339_from_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Current UTC time as RFC 3339.
pub fn now_rfc3339() -> String {
    rfc3339_from_millis(now_unix_millis())
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_timestamps() {
        assert_eq!(rfc3339_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_from_millis(1_609_459_200_000),
            "2021-01-01T00:00:00.000Z"
        );
        assert_eq!(
            rfc3339_from_millis(1_582_934_400_000),
            "2020-02-29T00:00:00.000Z"
        );
    }

    #[test]
    fn millis_and_time_of_day_round_trip() {
        let s = rfc3339_from_millis(1_609_459_261_500);
        assert_eq!(s, "2021-01-01T00:01:01.500Z");
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(now_unix_millis() > 1_600_000_000_000);
    }
}
