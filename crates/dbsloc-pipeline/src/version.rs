// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Build identity recorded in method logs and reconstruction files.

use serde_json::{json, Value};

pub const PRODUCT: &str = "Wyss DBS Localizer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_HASH: &str = env!("DBSLOC_GIT_HASH");

/// `"<version>+<git hash>"`, or just the version when the hash is unknown.
pub fn version_string() -> String {
    if GIT_HASH == "unknown" {
        VERSION.to_owned()
    } else {
        format!("{VERSION}+{GIT_HASH}")
    }
}

/// JSON object describing the build that produced an output file.
pub fn build_info() -> Value {
    json!({
        "product": PRODUCT,
        "version": VERSION,
        "git_hash": GIT_HASH,
        "target_os": std::env::consts::OS,
        "target_arch": std::env::consts::ARCH,
    })
}

/// [`build_info`] plus `written_at`: stamped into a file when it is created.
pub fn software_stamp() -> Value {
    let mut info = build_info();
    if let Some(obj) = info.as_object_mut() {
        obj.insert("written_at".into(), json!(utc_now_rfc3339()));
    }
    info
}

/// `{version, git_hash}`: stamped into a record that a later step adds to an existing file.
pub fn software_brief() -> Value {
    json!({"version": VERSION, "git_hash": GIT_HASH})
}

pub fn utc_now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_rfc3339(secs)
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (proleptic Gregorian calendar).
pub fn utc_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_carries_the_package_version() {
        assert!(version_string().starts_with(VERSION));
        assert_eq!(build_info()["version"], json!(VERSION));
        assert!(!GIT_HASH.is_empty());
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(utc_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_rfc3339(1_798_761_599), "2026-12-31T23:59:59Z");
        let stamp = software_stamp();
        assert_eq!(stamp["version"], json!(VERSION));
        assert!(stamp["written_at"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')));
        assert_eq!(software_brief()["git_hash"], json!(GIT_HASH));
    }
}
