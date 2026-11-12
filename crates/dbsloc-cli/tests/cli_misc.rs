// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, dbsloc, run, run_args, s};

const NOTE: &str = "dbsloc: note: HPDBS_* environment variables are ignored since the rename to Wyss DBS Localizer; use DBSLOC_* (found: HPDBS_A, HPDBS_B)";

#[test]
fn version_prints_the_package_version() {
    let out = run_args(&["version"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.trim().starts_with("dbsloc "), "{}", out.stdout);
    assert!(out.stdout.contains(env!("CARGO_PKG_VERSION")));
    let version_line = out.stdout.trim().to_owned();
    let out = run_args(&["--version"]);
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.trim(), version_line);
}

#[test]
fn legacy_environment_names_get_a_note() {
    let out = run(dbsloc()
        .env("HPDBS_B", "1")
        .env("HPDBS_A", "1")
        .arg("version"));
    assert!(out.stderr.contains(NOTE), "{}", out.stderr);
    assert_eq!(out.code, 0);
}

#[test]
fn no_note_without_legacy_names() {
    let out = run_args(&["version"]);
    assert!(!out.stderr.contains("HPDBS_"), "{}", out.stderr);
}

#[test]
fn ants_flags_are_stripped_before_parsing_and_warn_in_plan_mode() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "5",
        "--plan",
        "--log",
        "quiet",
        "--stages",
        "bids",
        "--ants-verbose",
        "1",
        "--ants-float",
        "1",
        "--ants-max-threads",
        "2",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("dbsloc: warning: some --ants-\u{2026} options have no effect on this run"),
        "{}",
        out.stderr
    );
    let out = run_args(&[
        "run", &rs, "5", "--plan", "--log", "quiet", "--stages", "bids",
    ]);
    assert!(!out.stderr.contains("--ants-"), "{}", out.stderr);
}

#[test]
fn a_non_integer_max_threads_value_is_left_to_the_parser() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = run_args(&[
        "run",
        &s(&root),
        "5",
        "--plan",
        "--ants-max-threads",
        "nope",
    ]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--ants-max-threads"), "{}", out.stderr);
}

#[test]
fn gpu_probe_json_has_the_documented_keys_and_the_exit_code_follows_availability() {
    let out = run_args(&["gpu", "probe", "--json"]);
    let v = out.json();
    let mut keys: Vec<&str> = v
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "adapter",
            "available",
            "backend",
            "detail",
            "device_type",
            "driver",
            "self_test_attempts",
            "software",
            "verdict"
        ]
    );
    let available = v["available"].as_bool().unwrap_or(false);
    assert_eq!(out.code, i32::from(!available));
    assert!(["GO", "BORDERLINE", "NO-GO", "UNAVAILABLE"]
        .contains(&v["verdict"].as_str().unwrap_or_default()));
}

#[test]
fn viz_checkreg_gen_requires_its_flags() {
    let out = run_args(&["viz", "checkreg", "gen"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--moving"), "{}", out.stderr);
    let help = run_args(&["viz", "checkreg", "gen", "--help"]);
    for f in [
        "--moving",
        "--fixed",
        "--out",
        "--no-preproc",
        "--lbound",
        "--ubound",
        "--max-slices",
    ] {
        assert!(help.stdout.contains(f), "{f}");
    }
}

#[test]
fn viz_scene_spawns_the_resolved_binary_and_forwards_its_exit_code() {
    let fake = env!("CARGO_BIN_EXE_dbsloc");
    let out = run(dbsloc().env("DBSLOC_GUI_BIN", fake).args([
        "viz",
        "scene",
        "--manifest",
        "m.json",
        "--headless-check",
    ]));
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("scene"), "{}", out.stderr);
}

#[test]
fn viz_checkreg_review_forwards_to_the_resolved_binary() {
    let fake = env!("CARGO_BIN_EXE_dbsloc");
    let out = run(dbsloc().env("DBSLOC_GUI_BIN", fake).args([
        "viz",
        "checkreg",
        "review",
        "--manifest",
        "m.json",
    ]));
    assert_eq!(out.code, 2);
}

#[test]
fn viz_checkreg_gen_runs_in_process_and_reports_a_missing_image() {
    let out = run_args(&[
        "viz",
        "checkreg",
        "gen",
        "--moving",
        "no_such_moving.nii",
        "--fixed",
        "no_such_fixed.nii",
        "--out",
        "o.png",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(!out.stderr.trim().is_empty());
}

#[test]
fn retired_viz_commands_are_gone() {
    for sub in ["mni-montage", "reco-electrode-qc"] {
        let out = run_args(&["viz", sub]);
        assert_eq!(out.code, 2, "{sub}");
    }
}
