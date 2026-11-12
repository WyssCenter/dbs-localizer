// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

pub(crate) const REAL_FIXTURES_VAR: &str = "DBSLOC_MEDTRONIC_FIXTURES";

pub(crate) fn real_fixture_dir(test: &str) -> Option<PathBuf> {
    let Some(value) = std::env::var_os(REAL_FIXTURES_VAR).filter(|v| !v.is_empty()) else {
        eprintln!("skipped {test}: {REAL_FIXTURES_VAR} is not set");
        return None;
    };
    let dir = PathBuf::from(value);
    assert!(
        dir.is_dir(),
        "{test}: {REAL_FIXTURES_VAR} points to {} which is not a folder",
        dir.display()
    );
    Some(dir)
}

pub(crate) fn synthetic_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/reports/synthetic")
}
