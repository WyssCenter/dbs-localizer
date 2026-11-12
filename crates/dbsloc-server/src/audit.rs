// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Append-only audit log (`<data_root>/server/audit.log`): one JSON object per line.
//!
//! No PHI beyond subject labels and file names, per `docs/SERVER_CLIENT_PLAN.md` §7.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::timeutil::now_rfc3339;

/// One audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: String,
    pub client: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEntry {
    /// Build a record with the current timestamp.
    pub fn now(
        client: impl Into<String>,
        dataset: Option<String>,
        subject: Option<String>,
        action: impl Into<String>,
        detail: Option<String>,
    ) -> AuditEntry {
        AuditEntry {
            ts: now_rfc3339(),
            client: client.into(),
            dataset,
            subject,
            action: action.into(),
            detail,
        }
    }

    /// Serialise one line, without the trailing newline.
    pub fn line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            format!(
                "{{\"ts\":\"{}\",\"client\":\"{}\",\"action\":\"{}\"}}",
                self.ts, self.client, self.action
            )
        })
    }
}

/// Append `entry` to `log_path`, creating the parent directory if needed.
pub fn append(log_path: &Path, entry: &AuditEntry) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(f, "{}", entry.line())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_one_json_object_without_a_newline() {
        let e = AuditEntry::now(
            "1.2.3.4:5555",
            Some("demo".to_owned()),
            Some("01".to_owned()),
            "run_submit",
            Some("id=abc".to_owned()),
        );
        let line = e.line();
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["client"], "1.2.3.4:5555");
        assert_eq!(v["dataset"], "demo");
        assert_eq!(v["subject"], "01");
        assert_eq!(v["action"], "run_submit");
        assert_eq!(v["detail"], "id=abc");
        assert!(v["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn append_creates_the_file_and_adds_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("server/audit.log");
        append(&p, &AuditEntry::now("c", None, None, "a", None)).unwrap();
        append(&p, &AuditEntry::now("c", None, None, "b", None)).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'));
        let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["action"], "a");
        assert!(!text.contains("null"));
    }
}
