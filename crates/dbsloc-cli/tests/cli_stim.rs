// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::Path;

use common::{dataset, run_args, s, write};
use dbsloc_pipeline::bids::resolve_subject_in;
use dbsloc_pipeline::vat::settings::{
    label_dir, write_stimparameters_mat, Contact, ModelKind, Polarity, Stimulation,
};
use serde_json::{json, Value};

const BOSTON: &str = "Boston Scientific Vercise Directed";

fn subject_dir(root: &Path) -> std::path::PathBuf {
    resolve_subject_in(root, None, "1", false)
        .unwrap_or_else(|e| panic!("{e}"))
        .subj_dir
}

fn with_reco(root: &Path) {
    dataset(root, "1");
    let sub = resolve_subject_in(root, None, "1", false).unwrap_or_else(|e| panic!("{e}"));
    let side = |x: f64| {
        json!({
            "coords_mm": [[x, 0.0, 0.0]],
            "trajectory": [[x, 0.0, -5.0]],
            "markers": {"head": [x, 0.0, -8.0], "tail": [x, 0.0, 6.0],
                        "x": [x + 0.65, 0.0, -8.0], "y": [x, 0.65, -8.0]},
        })
    };
    let reco = json!({
        "version": 2,
        "subj_id": "1",
        "mni": {"1": side(12.0), "2": side(-12.0)},
        "props": {"1": {"elmodel": BOSTON}, "2": {"elmodel": BOSTON}},
        "meta": {"side_keys": "1=right,2=left"},
    });
    let text = serde_json::to_string_pretty(&reco).unwrap_or_default();
    write(&sub.reconstruction_json(), text.as_bytes());
}

fn stimulation(label: &str) -> Stimulation {
    let mut st = Stimulation::blank(label, "1", ModelKind::Efield, [8, 8]);
    let src = &mut st.sides.right.sources[0];
    src.amplitude = 2.0;
    src.contacts[1] = Contact {
        index: 2,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 1.0,
    };
    st
}

fn write_json(path: &Path, st: &Stimulation) {
    let text = serde_json::to_string_pretty(st).unwrap_or_default();
    write(path, text.as_bytes());
}

#[test]
fn set_show_and_list_round_trip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    with_reco(dir.path());
    let input = dir.path().join("in.json");
    write_json(&input, &stimulation("first"));
    let root = s(dir.path());

    let out = run_args(&[
        "stim",
        "set",
        &root,
        "1",
        "first",
        "--from",
        &s(&input),
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["label"], "first");
    assert_eq!(v["contacts_per_side"], json!([8, 8]));
    let sd = subject_dir(dir.path());
    assert!(label_dir(&sd, "native", "first")
        .join("sub-1_desc-stimulation.json")
        .is_file());
    assert!(label_dir(&sd, "native", "first")
        .join("sub-1_desc-stimparameters.mat")
        .is_file());

    let out = run_args(&["stim", "show", &root, "1", "first", "--json", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["model"], "efield");
    assert_eq!(
        v["sides"]["right"]["sources"].as_array().map(Vec::len),
        Some(4)
    );

    let out = run_args(&["stim", "show", &root, "1", "first", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("right: source 1: current 2 mA"),
        "{}",
        out.stdout
    );

    let out = run_args(&["stim", "list", &root, "1", "--json", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v[0]["label"], "first");
    assert_eq!(v[0]["state"], "canonical");

    let out = run_args(&["stim", "list", &root, "1", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.lines().count() == 2, "{}", out.stdout);
}

#[test]
fn set_rejects_invalid_settings_with_the_spec_code_and_exit_1() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    with_reco(dir.path());
    let mut st = stimulation("bad");
    st.sides.right.sources[0].amplitude = 1.0;
    st.sides.right.sources[0].contacts[1].percent = 40.0;
    let input = dir.path().join("bad.json");
    write_json(&input, &st);
    let root = s(dir.path());
    let out = run_args(&[
        "stim",
        "set",
        &root,
        "1",
        "bad",
        "--from",
        &s(&input),
        "--no-index",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(
        out.stderr.contains("vat: E_VAT_settings: right source 1:"),
        "{}",
        out.stderr
    );
    let sd = subject_dir(dir.path());
    assert!(!label_dir(&sd, "native", "bad").exists());
}

#[test]
fn set_rejects_a_label_that_differs_from_the_file() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    with_reco(dir.path());
    let input = dir.path().join("in.json");
    write_json(&input, &stimulation("inside"));
    let out = run_args(&[
        "stim",
        "set",
        &s(dir.path()),
        "1",
        "outside",
        "--from",
        &s(&input),
        "--no-index",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("E_VAT_settings"), "{}", out.stderr);
}

#[test]
fn import_reads_a_stimparameters_mat_and_lists_the_folder_as_importable() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    with_reco(dir.path());
    let sd = subject_dir(dir.path());
    let mat = label_dir(&sd, "MNI152NLin2009bAsym", "lead1").join("sub-1_desc-stimparameters.mat");
    write_stimparameters_mat(&mat, &stimulation("lead1"), [0.0, 0.0])
        .unwrap_or_else(|e| panic!("{e}"));
    let root = s(dir.path());

    let out = run_args(&["stim", "list", &root, "1", "--json", "--no-index"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()[0]["state"], "importable");

    let out = run_args(&[
        "stim",
        "import",
        &root,
        "1",
        &s(&mat),
        "--label",
        "imported",
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_args(&[
        "stim",
        "show",
        &root,
        "1",
        "imported",
        "--json",
        "--no-index",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v: Value = out.json();
    assert_eq!(v["label"], "imported");
    assert_eq!(v["sides"]["right"]["sources"][0]["amplitude"], 2.0);
    assert_eq!(
        v["sides"]["right"]["sources"][0]["contacts"][1]["polarity"],
        "cathode"
    );
    assert!(v["provenance"]["imported_from"]["sha256"].is_string());
}

#[test]
fn import_of_a_missing_file_exits_1_and_a_missing_subject_exits_2() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    with_reco(dir.path());
    let root = s(dir.path());
    let missing = s(&dir.path().join("nope.mat"));
    let out = run_args(&["stim", "import", &root, "1", &missing, "--no-index"]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let out = run_args(&["stim", "list", &root, "99", "--no-index"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}
