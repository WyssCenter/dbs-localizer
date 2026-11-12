// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, results, run_args, s, write};
use serde_json::Value;

fn subjects(v: &Value) -> Vec<Value> {
    v["subjects"].as_array().cloned().unwrap_or_default()
}

fn ids(v: &Value) -> Vec<String> {
    subjects(v)
        .iter()
        .map(|e| e["subj_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn phases(v: &Value) -> Vec<String> {
    subjects(v)
        .iter()
        .map(|e| e["phase"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn two_subjects_plan_one_summary_per_subject() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    dataset(dir.path(), "02");
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "01",
        "sub-02",
        "--plan",
        "--stages",
        "bids,preprocess",
        "--log",
        "plain",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["batch"], true);
    assert_eq!(ids(&v), ["01", "02"]);
    assert_eq!(phases(&v), ["done", "done"]);
    for e in subjects(&v) {
        assert_eq!(e["apply"], false);
        assert_eq!(e["stages"], serde_json::json!(["bids", "preprocess"]));
        assert_eq!(
            results(&e),
            [
                ("bids".to_owned(), "plan:bids".to_owned()),
                ("preprocess".to_owned(), "plan:preprocess".to_owned())
            ]
        );
        assert!(e["locks_dir"]
            .as_str()
            .unwrap_or_default()
            .ends_with("locks"));
    }
    assert_eq!(v["totals"]["subjects"], 2);
    assert_eq!(v["totals"]["done"], 2);
    assert_eq!(v["totals"]["stages_skipped"], 4);
    assert_eq!(v["awaiting_qc"], serde_json::json!([]));
    assert!(out.stderr.contains("Subject 1/2: sub-01"), "{}", out.stderr);
    assert!(out.stderr.contains("Subject 2/2: sub-02"), "{}", out.stderr);
    assert!(
        out.stderr.contains("Batch summary: 2 subject(s)"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("| ok/failed/skipped |"),
        "{}",
        out.stderr
    );
}

#[test]
fn one_subject_keeps_the_single_run_object() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    let rs = s(&root);
    let out = run_args(&[
        "run", &rs, "01", "--plan", "--stages", "bids", "--log", "quiet",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert!(v.get("batch").is_none());
    assert_eq!(v["subj_id"], "01");
}

#[test]
fn a_bogus_id_is_a_plan_error_and_the_others_still_run() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    dataset(dir.path(), "03");
    let rs = s(&root);
    let out = run_args(&[
        "run", &rs, "01", "bogus", "03", "--plan", "--stages", "bids", "--log", "quiet",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let v = out.json();
    assert_eq!(ids(&v), ["01", "bogus", "03"]);
    assert_eq!(phases(&v), ["done", "plan_error", "done"]);
    assert!(subjects(&v)[1]["message"].as_str().is_some());
    assert_eq!(v["totals"]["plan_error"], 1);
    assert!(
        out.stderr.contains("sub-bogus failed") && out.stderr.contains("Moving on"),
        "{}",
        out.stderr
    );
}

#[test]
fn only_bogus_ids_exit_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    let rs = s(&root);
    let out = run_args(&["run", &rs, "x", "y", "--plan", "--log", "quiet"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert_eq!(phases(&out.json()), ["plan_error", "plan_error"]);
}

#[test]
fn stop_on_first_failure_cancels_the_rest() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    dataset(dir.path(), "03");
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "bogus",
        "01",
        "03",
        "--plan",
        "--stages",
        "bids",
        "--log",
        "quiet",
        "--stop-on-first-failure",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let v = out.json();
    assert_eq!(phases(&v), ["plan_error", "cancelled", "cancelled"]);
    assert_eq!(v["totals"]["cancelled"], 2);
}

#[test]
fn all_subjects_and_a_subjects_file_expand_in_list_order() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "b");
    dataset(dir.path(), "a");
    dataset(dir.path(), "c");
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "--all-subjects",
        "--plan",
        "--stages",
        "bids",
        "--log",
        "quiet",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(ids(&out.json()), ["a", "b", "c"]);

    let list = dir.path().join("cohort.txt");
    write(&list, b"# cohort\nsub-c\n\na\nc\n");
    let ls = s(&list);
    let out = run_args(&[
        "run",
        &rs,
        "b",
        "--subjects-file",
        &ls,
        "--plan",
        "--stages",
        "bids",
        "--log",
        "quiet",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(ids(&out.json()), ["b", "c", "a"]);
}

#[test]
fn usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "01");
    let rs = s(&root);
    let out = run_args(&["run", &rs, "--plan"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("--all-subjects"), "{}", out.stderr);
    let out = run_args(&["run", &rs, "01", "--all-subjects", "--plan"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let missing = s(&dir.path().join("missing.txt"));
    let out = run_args(&["run", &rs, "--subjects-file", &missing, "--plan"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let out = run_args(&[
        "run",
        &rs,
        "01",
        "--continue-on-error",
        "--stop-on-first-failure",
        "--plan",
    ]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let empty = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let es = s(empty.path());
    let out = run_args(&["run", &es, "--all-subjects", "--plan"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}
