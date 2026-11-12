// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, run_args, s, write};

#[test]
fn subject_show_json() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    let out = run_args(&["subject", "show", &s(&root), "22"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let d = out.json();
    assert_eq!(d["subj_id"], "22");
    assert_eq!(d["anchor_modality"], "T1w");
    assert_eq!(d["postop_modality"], "CT");
    assert!(d["preop_raw"].get("T1w").is_some());
    assert!(d.get("primary_template").is_none());
}

#[test]
fn subject_show_accepts_the_sub_prefix_and_reports_spacedef() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    write(
        &root.join("spacedef.json"),
        br#"{"name": "MNI_test", "norm_mapping": {"T1w": "t1"}, "misfit_template": "t2"}"#,
    );
    let out = run_args(&["subject", "show", &s(&root), "sub-22", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let d = out.json();
    assert_eq!(d["subj_id"], "22");
    assert_eq!(d["primary_template"], "t1");
    assert_eq!(d["space_name"], "MNI_test");
}

#[test]
fn subject_show_missing_subject_exits_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["subject", "show", &s(dir.path()), "7"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("rawdata"), "{}", out.stderr);
}

#[test]
fn malformed_spacedef_exits_1() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "22");
    write(&root.join("spacedef.json"), b"[1, 2]");
    let out = run_args(&["subject", "show", &s(&root), "22"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("spacedef"), "{}", out.stderr);
}
