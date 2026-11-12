// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::{dataset, dbsloc, result_of, results, run, run_args, s, t1_of, write};

fn plan(root: &std::path::Path, extra: &[&str]) -> common::Out {
    let rs = s(root);
    let mut a = vec!["run", rs.as_str(), "5", "--plan", "--log", "quiet"];
    a.extend_from_slice(extra);
    run_args(&a)
}

#[test]
fn plan_lists_every_default_stage_with_plan_tokens() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &[]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["subj_id"], "5");
    assert_eq!(v["apply"], false);
    assert_eq!(v["fingerprint_locks"], false);
    let stages: Vec<String> = v["stages"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(stages.len(), 10);
    assert_eq!(stages[0], "bids");
    for (st, r) in results(&v) {
        assert_eq!(r, format!("plan:{st}"));
    }
    assert!(v["results"][0]["seconds"].is_number());
    assert!(v["locks_dir"]
        .as_str()
        .unwrap_or_default()
        .ends_with("locks"));
    assert!(v.get("total_seconds").is_none());
    assert!(v.get("lock_input_paths").is_none());
}

#[test]
fn json_summary_for_a_two_stage_run() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &["--no-index", "--stages", "bids,preprocess"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["stages"], serde_json::json!(["bids", "preprocess"]));
    assert_eq!(result_of(&v, "preprocess"), "plan:preprocess");
}

#[test]
fn no_flags_drop_stages_and_unknown_names_exit_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &["--no-coreg", "--no-vat", "--no-brainshift"]);
    let st = out.json()["stages"].to_string();
    for gone in ["\"coreg\"", "\"precoreg\"", "\"vat\"", "\"brainshift\""] {
        assert!(!st.contains(gone), "{st}");
    }
    let rs = s(&root);
    let out = run_args(&["run", &rs, "5", "--plan", "--stages", "bids,nonsense"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("nonsense"), "{}", out.stderr);
}

#[test]
fn coregister_alias_is_accepted_and_reported_by_its_canonical_name() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &["--stages", "coregister"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["stages"], serde_json::json!(["coreg"]));
    assert_eq!(result_of(&v, "coreg"), "plan:coreg");
    let out = plan(&root, &["--stages", "coreg,coregister"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("more than once"), "{}", out.stderr);
}

#[test]
fn run_exits_2_on_a_bids_error() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["run", &s(dir.path()), "7", "--no-index"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("dbsloc:"), "{}", out.stderr);
    assert!(out.stderr.contains("rawdata"), "{}", out.stderr);
    assert!(out.stdout.is_empty());
}

#[test]
fn executed_stubs_and_missing_electrode_exit_1() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    write(
        &root
            .join("derivatives/leaddbs/sub-5/normalization/anat")
            .join("sub-5_ses-postop_space-MNI152NLin2009bAsym_desc-preproc_CT.nii.gz"),
        b"x",
    );
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "5",
        "--log",
        "quiet",
        "--no-checkreg",
        "--headless",
        "--stages",
        "bids,orientation,vat,reconstruct",
    ]);
    let v = out.json();
    assert_eq!(result_of(&v, "bids"), "plan:bids");
    assert!(result_of(&v, "orientation").starts_with("error: orientation: missing Reco JSON: "));
    assert_eq!(result_of(&v, "vat"), "skip:vat: no stimulation settings");
    assert!(result_of(&v, "reconstruct").starts_with("error:"));
    assert_eq!(v["apply"], true);
    assert_eq!(out.code, 1);
}

#[test]
fn an_approved_lock_skips_the_stage_until_the_input_changes() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let t1 = t1_of(&root, "5");
    let rs = s(&root);
    let a = run_args(&["lock", "approve", &rs, "5", "bids", "--input", &s(&t1)]);
    assert_eq!(a.code, 0, "{}", a.stderr);
    let args = ["--stages", "bids", "--fingerprint-locks"];
    let v = plan(&root, &args).json();
    assert_eq!(result_of(&v, "bids"), "skip:approved");
    assert_eq!(v["fingerprint_locks"], true);
    let listed = v["lock_input_paths"]["bids"][0]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(listed.ends_with("sub-5_ses-preop_T1w.nii.gz"), "{listed}");
    write(&t1, b"changed");
    let v = plan(&root, &args).json();
    assert_eq!(result_of(&v, "bids"), "plan:bids");
    let v = plan(&root, &["--stages", "bids", "--overwrite-approved"]).json();
    assert_eq!(result_of(&v, "bids"), "plan:bids");
}

#[test]
fn progress_goes_to_stderr_and_profile_adds_timings() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let rs = s(&root);
    let out = run_args(&[
        "run",
        &rs,
        "5",
        "--plan",
        "--log",
        "plain",
        "--stages",
        "bids,coreg",
        "--profile",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("Starting:"), "{}", out.stderr);
    assert!(out
        .stderr
        .contains("Running pipeline for subject 5 (2 steps)"));
    assert!(out.stderr.contains("May take several minutes."));
    assert!(out.stderr.contains("Pipeline finished all 2 step(s)"));
    assert!(!out.stderr.contains("step(s) failed"), "{}", out.stderr);
    assert!(!out.stdout.contains("Starting:"));
    let v = out.json();
    let timings = v["stage_timings_sec"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let keys: Vec<&String> = timings.keys().collect();
    assert_eq!(keys, ["bids", "coreg"]);
    assert!(v["total_seconds"].is_number());
}

#[test]
fn quiet_prints_no_progress() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &["--stages", "bids"]);
    assert_eq!(out.code, 0);
    assert!(!out.stderr.contains("Starting:"), "{}", out.stderr);
}

#[test]
fn removed_cprofile_flags_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(&root, &["--stages", "bids", "--cprofile"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}

#[test]
fn template_flag_accepts_a_catalogue_id_in_plan_mode() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let out = plan(
        &root,
        &[
            "--stages",
            "bids",
            "--template",
            "MNI152NLin2009bAsym_res-1mm",
            "--no-norm-multispectral",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn headless_env_and_checkreg_env_are_accepted() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = dataset(dir.path(), "5");
    let rs = s(&root);
    let out = run(dbsloc()
        .env("DBSLOC_HEADLESS", "1")
        .env("DBSLOC_OPEN_CHECKREG", "0")
        .env("DBSLOC_STOP_ON_ERROR", "1")
        .args([
            "run", &rs, "5", "--plan", "--log", "quiet", "--stages", "bids",
        ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
}
