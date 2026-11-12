// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dbsloc, run, s};

const IDS: [&str; 2] = [
    "edlow2019-7t-exvivo_res-0.5mm",
    "edlow2019-7t-exvivo_res-0.2mm_desc-deepbrain",
];

fn with_data_dir(args: &[&str]) -> (common::Out, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args(args));
    (out, data)
}

#[test]
fn list_contains_both_ids() {
    let (out, _d) = with_data_dir(&["backdrop", "list"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for id in IDS {
        assert!(out.stdout.contains(id), "{id}");
    }
    assert!(out
        .stdout
        .lines()
        .next()
        .unwrap_or_default()
        .starts_with("id"));
}

#[test]
fn list_json_marks_every_entry_available() {
    let (out, _d) = with_data_dir(&["backdrop", "list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    let rows = v.as_array().cloned().unwrap_or_default();
    let ids: Vec<&str> = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, IDS);
    assert!(rows.iter().all(|r| r["status"] == "available"));
}

#[test]
fn fetch_unknown_id_exits_2_and_lists_valid_ids() {
    let (out, _d) = with_data_dir(&["backdrop", "fetch", "nope"]);
    assert_eq!(out.code, 2);
    for id in IDS {
        assert!(out.stderr.contains(id), "{id}");
    }
}
