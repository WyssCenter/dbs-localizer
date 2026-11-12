// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::Path;

use common::{dbsloc, run, run_args, s, write};
use dbsloc_pipeline::bids::{resolve_subject_in, resolve_subject_with};
use dbsloc_pipeline::derivatives;
use serde_json::{json, Value};

const BOSTON: &str = "Boston Scientific Vercise Directed";

fn mri_dataset(root: &Path) -> std::path::PathBuf {
    mri_dataset_in(root, None)
}

fn mri_dataset_in(root: &Path, output_dir: Option<&Path>) -> std::path::PathBuf {
    write(
        &root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
        b"x",
    );
    write(
        &root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_acq-ax_T1w.nii.gz"),
        b"x",
    );
    let sub = resolve_subject_in(root, output_dir, "1", false).unwrap_or_else(|e| panic!("{e}"));
    let space = "MNI152NLin2009bAsym";
    write(
        &derivatives::norm_method_log(&sub),
        format!(r#"{{"template_space": "{space}"}}"#).as_bytes(),
    );
    write(&derivatives::norm_postop_image(&sub, "ax_MRI", space), b"x");
    let side = |x: f64, roll_deg: f64| {
        let (s, c) = roll_deg.to_radians().sin_cos();
        json!({
            "coords_mm": [[x, 0.0, 0.0]],
            "trajectory": [[x, 0.0, -5.0]],
            "markers": {
                "head": [x, 0.0, -8.0],
                "tail": [x, 0.0, 6.0],
                "x": [x + 0.65 * c, 0.65 * s, -8.0],
                "y": [x - 0.65 * s, 0.65 * c, -8.0],
            },
            "trajvector": [0.0, 0.0, 1.0],
        })
    };
    let sub = resolve_subject_in(root, output_dir, "1", false).unwrap_or_else(|e| panic!("{e}"));
    let inputs = dbsloc_pipeline::locks::snap_input_paths(
        &dbsloc_pipeline::manual_correction::correction_input_paths(&sub),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let reco = json!({
        "version": 2,
        "subj_id": "1",
        "mni": {"1": side(12.0, 10.0), "2": side(-12.0, 0.0)},
        "props": {
            "1": {"elmodel": BOSTON, "manually_corrected": false},
            "2": {"elmodel": BOSTON, "manually_corrected": false},
        },
        "meta": {
            "orientation": {"1": {"status": "resolved", "roll_deg": 10.0}, "2": "unresolved"},
            "reconstruct": {"status": {"1": "traced", "2": "traced"}, "inputs": inputs},
        },
    });
    let text = serde_json::to_string_pretty(&reco).unwrap_or_default() + "\n";
    write(&sub.reconstruction_json(), text.as_bytes());
    sub.reconstruction_json()
}

#[test]
fn reco_show_prints_the_edit_space_and_sides() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let out = run_args(&["reco", "show", &s(dir.path()), "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["space"], "mni");
    assert_eq!(v["sides"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["sides"][0]["label"], "Right lead");
    let roll = v["sides"][0]["roll_deg"].as_f64().unwrap_or(f64::NAN);
    assert!((roll - 10.0).abs() < 1e-9, "{roll}");
    assert_eq!(v["sides"][0]["manually_corrected"], false);
}

#[test]
fn reco_show_and_correct_read_and_write_the_chosen_output_folder() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out_dir = dir.path().join("results");
    let reco = mri_dataset_in(dir.path(), Some(&out_dir));
    assert!(reco.starts_with(dbsloc_pipeline::paths::absolute(&out_dir)));
    let root = s(dir.path());
    let missing = run_args(&["reco", "show", &root, "1", "--no-index"]);
    assert_ne!(missing.code, 0, "{}", missing.stdout);
    let out = run_args(&[
        "reco",
        "show",
        &root,
        "1",
        "--no-index",
        "--output-dir",
        &s(&out_dir),
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["sides"].as_array().map(Vec::len), Some(2));
    let before = std::fs::read_to_string(&reco).unwrap_or_default();
    let out = run(dbsloc()
        .env("DBSLOC_OUTPUT_DIR", &out_dir)
        .args(correct_args(&root, "right", "12,0,-7", &[])));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_ne!(std::fs::read_to_string(&reco).unwrap_or_default(), before);
    assert!(!dir.path().join("derivatives").exists());
}

#[test]
fn reco_correct_rewrites_a_template_only_file() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let reco = mri_dataset(dir.path());
    let out = run_args(&[
        "reco",
        "correct",
        &s(dir.path()),
        "1",
        "--side",
        "right",
        "--head",
        "12,0,-7",
        "--tail",
        "12,0,7",
        "--roll",
        "-20",
        "--space",
        "mni",
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let report = out.json();
    assert_eq!(report["sides"][0]["key"], "1");
    assert_eq!(report["sides"][0]["space"], "mni");
    assert_eq!(report["sides"][0]["roll_deg"], -20.0);
    let after: Value = serde_json::from_str(&std::fs::read_to_string(reco).unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(after["props"]["1"]["manually_corrected"], true);
    assert_eq!(after["props"]["2"]["manually_corrected"], false);
    assert_eq!(after["meta"]["orientation"]["1"]["status"], "manual");
    assert_eq!(after["meta"]["manual_correction"]["1"]["source"], "cli");

    let out = run_args(&["reco", "show", &s(dir.path()), "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let shown = out.json();
    assert_eq!(shown["sides"][0]["manually_corrected"], true);
    let roll = shown["sides"][0]["roll_deg"].as_f64().unwrap_or(f64::NAN);
    assert!((roll + 20.0).abs() < 1e-9, "{roll}");
}

#[test]
fn reco_correct_rejects_a_bogus_space() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let out = run_args(&[
        "reco",
        "correct",
        &s(dir.path()),
        "1",
        "--side",
        "left",
        "--head",
        "-12,0,-7",
        "--tail",
        "-12,0,7",
        "--space",
        "scanner",
        "--no-index",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_RECON_manual"), "{}", out.stderr);
}

#[test]
fn reco_correct_rejects_native_space_on_a_template_only_file() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let out = run_args(&[
        "reco",
        "correct",
        &s(dir.path()),
        "1",
        "--side",
        "left",
        "--head",
        "-12,0,-7",
        "--tail",
        "-12,0,7",
        "--space",
        "native",
        "--no-index",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_RECON_manual"), "{}", out.stderr);
}

#[test]
fn reco_for_a_missing_subject_exits_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["reco", "show", &s(dir.path()), "9"]);
    assert_eq!(out.code, 2);
}

fn correct_args<'a>(
    root: &'a str,
    side: &'a str,
    head: &'a str,
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut v = vec![
        "reco",
        "correct",
        root,
        "1",
        "--side",
        side,
        "--head",
        head,
        "--tail",
        "12,0,7",
        "--no-index",
    ];
    v.extend_from_slice(extra);
    v
}

#[test]
fn reco_correct_rejects_an_unknown_side() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let root = s(dir.path());
    let out = run_args(&correct_args(&root, "middle", "12,0,-7", &[]));
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_RECON_manual"), "{}", out.stderr);
}

#[test]
fn reco_correct_rejects_a_non_finite_head() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let reco = mri_dataset(dir.path());
    let before = std::fs::read(&reco).unwrap_or_default();
    let root = s(dir.path());
    for head in ["12,nan,-7", "inf,0,-7", "12,0"] {
        let out = run_args(&correct_args(&root, "right", head, &[]));
        assert_eq!(out.code, 1, "{head}");
        assert!(out.stderr.contains("E_RECON_manual"), "{}", out.stderr);
    }
    assert_eq!(std::fs::read(&reco).unwrap_or_default(), before);
}

#[test]
fn reco_correct_defaults_to_the_edit_space_of_the_file() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let root = s(dir.path());
    let out = run_args(&correct_args(&root, "right", "12,0,-7", &[]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["sides"][0]["space"], "mni");
}

#[test]
fn reco_show_works_without_the_backdrop_image() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let sub = resolve_subject_with(dir.path(), "1", false).unwrap_or_else(|e| panic!("{e}"));
    let image = derivatives::norm_postop_image(&sub, "ax_MRI", "MNI152NLin2009bAsym");
    std::fs::remove_file(&image).unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["reco", "show", &s(dir.path()), "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["space"], "mni");
    assert!(v["backdrop_nii"]
        .as_str()
        .is_some_and(|p| p.ends_with("desc-preproc_T1w.nii.gz")));
}

fn lock_approval(root: &Path, stage: &str) -> Option<bool> {
    let sub = resolve_subject_with(root, "1", false).unwrap_or_else(|e| panic!("{e}"));
    let text = std::fs::read_to_string(sub.locks_dir().join(format!("{stage}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["approval"].as_bool()
}

#[test]
fn reco_correct_approves_the_step_only_once_every_lead_is_reviewed() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let reco = mri_dataset(dir.path());
    let root = s(dir.path());
    let out = run_args(&correct_args(&root, "right", "12,0,-7", &["--roll", "15"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_ne!(lock_approval(dir.path(), "reconstruct"), Some(true));
    assert!(out.json()["approved"]
        .as_array()
        .is_some_and(|a| !a.iter().any(|s| s == "reconstruct")));

    let out = run_args(&[
        "reco",
        "correct",
        &root,
        "1",
        "--accept",
        "left",
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let report = out.json();
    assert_eq!(report["sides"][0]["key"], "2");
    assert_eq!(report["sides"][0]["review"], "accepted");
    assert_eq!(lock_approval(dir.path(), "reconstruct"), Some(true));
    assert_ne!(lock_approval(dir.path(), "orientation"), Some(true));
    let after: Value = serde_json::from_str(&std::fs::read_to_string(reco).unwrap_or_default())
        .unwrap_or_default();
    assert_eq!(after["props"]["2"]["manually_corrected"], false);
    assert_eq!(
        after["meta"]["manual_correction"]["1"]["review"],
        "corrected"
    );
    assert_eq!(
        after["meta"]["manual_correction"]["1"]["roll_review"],
        "set"
    );
    assert_eq!(
        after["meta"]["manual_correction"]["2"]["review"],
        "accepted"
    );
    assert!(after["meta"]["manual_correction"]["2"]["roll_review"].is_null());

    let out = run_args(&["reco", "show", &root, "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["sides"][1]["review"], "accepted");
}

#[test]
fn reco_correct_needs_something_to_do() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    mri_dataset(dir.path());
    let root = s(dir.path());
    let out = run_args(&["reco", "correct", &root, "1", "--no-index"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("nothing to do"), "{}", out.stderr);
    let out = run_args(&[
        "reco",
        "correct",
        &root,
        "1",
        "--side",
        "right",
        "--no-index",
    ]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("--side, --head and --tail"),
        "{}",
        out.stderr
    );
}
