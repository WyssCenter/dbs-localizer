// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dbsloc, run, run_args, s};

const IDS: [&str; 7] = [
    "MNI152NLin2009bAsym_res-1mm",
    "MNI152NLin2009bAsym_res-0.5mm",
    "MNI152NLin2009bSym_res-0.5mm",
    "MNI152NLin2009aAsym_res-1mm",
    "MNI152NLin2009aSym_res-1mm",
    "MNI152NLin2009cAsym_res-1mm",
    "MNI152NLin2009cSym_res-1mm",
];

fn with_data_dir(args: &[&str]) -> (common::Out, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args(args));
    (out, data)
}

#[test]
fn list_contains_all_ids() {
    let (out, _d) = with_data_dir(&["template", "list"]);
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
fn list_json_marks_the_bundled_entry() {
    let (out, _d) = with_data_dir(&["template", "list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    let rows = v.as_array().cloned().unwrap_or_default();
    let ids: Vec<&str> = rows
        .iter()
        .map(|r| r["id"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(ids, IDS);
    let first = rows[0]["status"].as_str().unwrap_or_default();
    assert!(first == "bundled" || first == "missing", "{first}");
    assert!(rows[1..].iter().all(|r| r["status"] == "available"));
}

#[test]
fn fetch_unknown_id_exits_2_and_lists_valid_ids() {
    let (out, _d) = with_data_dir(&["template", "fetch", "nope"]);
    assert_eq!(out.code, 2);
    for id in IDS {
        assert!(out.stderr.contains(id), "{id}");
    }
}

#[test]
fn fetch_of_the_bundled_entry_fails_without_network() {
    let (out, _d) = with_data_dir(&["template", "fetch", IDS[0]]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("dbsloc: error:"), "{}", out.stderr);
    assert!(out.stdout.is_empty());
}

#[test]
fn no_arguments_prints_help() {
    let out = run_args(&[]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("Usage: dbsloc"), "{}", out.stdout);
}
