// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, dbsloc, run, run_args, s, t1_of};

#[test]
fn lock_approve_and_show_round_trip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "2");
    let t1 = t1_of(&root, "2");
    let rs = s(&root);
    let ts = s(&t1);
    let out = run_args(&[
        "lock",
        "approve",
        &rs,
        "2",
        "preprocess",
        "--input",
        &ts,
        "--method",
        "N4",
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert!(std::path::Path::new(v["wrote"].as_str().unwrap_or_default()).is_file());
    assert_eq!(v["stage"], "preprocess");
    assert_eq!(v["input_count"], 1);

    let out = run_args(&["lock", "show", &rs, "2", "preprocess", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let d = out.json();
    assert_eq!(d["lock"]["method"], "N4");
    assert_eq!(d["lock"]["approval"], true);
}

#[test]
fn locks_follow_the_output_folder_flag_then_the_variable() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "2");
    let rs = s(&root);
    let flag_dir = dir.path().join("flag out");
    let env_dir = dir.path().join("env_out");
    let out = run_args(&[
        "lock",
        "approve",
        &rs,
        "2",
        "preprocess",
        "--no-index",
        "--output-dir",
        &s(&flag_dir),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let wrote = std::path::PathBuf::from(out.json()["wrote"].as_str().unwrap_or_default());
    assert!(wrote.is_file());
    assert!(wrote.starts_with(dbsloc_pipeline::paths::absolute(&flag_dir)));
    assert!(!root.join("derivatives").exists());

    let out = run(dbsloc().env("DBSLOC_OUTPUT_DIR", &env_dir).args([
        "lock",
        "approve",
        &rs,
        "2",
        "coreg",
        "--no-index",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(env_dir.join("sub-2/locks/coreg.json").is_file());

    let show = |env: Option<&std::path::Path>, extra: &[&str]| {
        let mut c = dbsloc();
        if let Some(e) = env {
            c.env("DBSLOC_OUTPUT_DIR", e);
        }
        c.args(["lock", "show", &rs, "2", "preprocess", "--no-index"]);
        c.args(extra);
        run(&mut c).json()["lock"]["approval"].as_bool()
    };
    assert_eq!(show(None, &["--output-dir", &s(&flag_dir)]), Some(true));
    assert_eq!(show(Some(&env_dir), &[]), None);
    assert_eq!(
        show(Some(&env_dir), &["--output-dir", &s(&flag_dir)]),
        Some(true)
    );
    assert_eq!(show(None, &[]), None);
}

#[test]
fn lock_show_without_a_lock_is_null() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "2");
    let out = run_args(&["lock", "show", &s(&root), "2", "vat", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.json()["lock"].is_null());
}

#[test]
fn lock_for_a_missing_subject_exits_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["lock", "show", &s(dir.path()), "9", "vat"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.starts_with("dbsloc:"), "{}", out.stderr);
}
