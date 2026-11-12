// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::Path;

use common::{dbsloc, run, s, write};

fn fixture(base: &Path) {
    let a = base.join("templates/space/MNI152NLin2009bAsym/atlases");
    write(&a.join("AtlasOne/lh/a.nii.gz"), b"12345");
    write(&a.join("AtlasTwo/rh/b.nii.gz"), b"1234567");
    write(&base.join("connectomes/dMRI/Xconn/data.mat"), b"12345678");
    write(
        &base.join("connectomes/fMRI/Yconn/dataset_info.json"),
        b"{}",
    );
}

fn with_env(args: &[&str]) -> (common::Out, tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    fixture(src.path());
    let mut full: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    full.insert(2, s(src.path()));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args(&full));
    (out, src, data)
}

#[test]
fn scan_prints_both_tables_and_the_link_lines() {
    let (out, src, _d) = with_env(&["leaddbs", "scan"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for word in [
        "Atlases (2",
        "AtlasOne",
        "AtlasTwo",
        "Connectomes (2",
        "Xconn",
        "Yconn",
        "Structural",
        "Functional",
    ] {
        assert!(out.stdout.contains(word), "{word}\n{}", out.stdout);
    }
    assert!(out.stdout.contains("DBSLOC_LEADDBS_ATLASES_DIR="));
    assert!(out.stdout.contains("DBSLOC_LEADDBS_CONNECTOMES_DIR="));
    assert!(src.path().join("connectomes").is_dir());
}

#[test]
fn scan_json_has_items_and_link() {
    let (out, _s, _d) = with_env(&["leaddbs", "scan", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["atlases"].as_array().map(Vec::len), Some(2));
    assert_eq!(v["connectomes"][0]["class"], "dMRI");
    assert_eq!(v["connectomes"][0]["bytes"], 8);
    assert!(v["link"]["atlases_dir"].is_string());
}

#[test]
fn scan_of_an_empty_folder_exits_0_with_the_note() {
    let empty = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "scan",
        &s(empty.path()),
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out
        .stdout
        .contains("No Lead-DBS atlases or connectomes were found"));
}

#[test]
fn scan_of_a_missing_folder_exits_1() {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "scan",
        &s(&data.path().join("nope")),
    ]));
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_LDBS_MISSING"), "{}", out.stderr);
}

#[test]
fn copy_all_copies_into_the_user_data_and_root() {
    let src = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    fixture(src.path());
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "copy",
        &s(src.path()),
        "--all",
        "--json",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["copied"].as_array().map(Vec::len), Some(4));
    assert!(data
        .path()
        .join("atlases/leaddbs/AtlasOne/lh/a.nii.gz")
        .is_file());
    assert!(data
        .path()
        .join("connectomes/dMRI/Xconn/data.mat")
        .is_file());
    assert!(data
        .path()
        .join("connectomes/fMRI/Yconn/dataset_info.json")
        .is_file());
}

#[test]
fn copy_with_a_selection_and_root() {
    let src = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    fixture(src.path());
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "copy",
        &s(src.path()),
        "--atlas",
        "AtlasTwo",
        "--connectome",
        "Xconn",
        "--root",
        &s(root.path()),
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(data.path().join("atlases/leaddbs/AtlasTwo").is_dir());
    assert!(!data.path().join("atlases/leaddbs/AtlasOne").exists());
    assert!(root.path().join("dMRI/Xconn/data.mat").is_file());
    assert!(!root.path().join("fMRI").exists());
}

#[test]
fn copy_into_a_data_folder_inside_the_lead_dbs_folder_is_refused() {
    let src = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    fixture(src.path());
    let inside = src.path().join("mydata");
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(&inside)).args([
        "leaddbs",
        "copy",
        &s(src.path()),
        "--all",
    ]));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stderr.contains("E_LDBS_IO"), "{}", out.stderr);
    assert!(
        out.stderr.contains("is inside the Lead-DBS folder"),
        "{}",
        out.stderr
    );
    assert!(!inside.exists());
}

#[test]
fn copy_unknown_atlas_exits_2_and_lists_names() {
    let src = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    fixture(src.path());
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "copy",
        &s(src.path()),
        "--atlas",
        "Nope",
    ]));
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("AtlasOne") && out.stderr.contains("AtlasTwo"),
        "{}",
        out.stderr
    );
    assert!(!data.path().join("atlases").exists());
}

#[test]
fn scan_json_of_a_missing_folder_prints_an_error_object_and_exits_1() {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = run(dbsloc().env("DBSLOC_DATA_DIR", s(data.path())).args([
        "leaddbs",
        "scan",
        &s(&data.path().join("nope")),
        "--json",
    ]));
    assert_eq!(out.code, 1);
    let text = out.json()["error"].as_str().unwrap_or("").to_owned();
    assert!(text.starts_with("E_LDBS_MISSING: "), "{text}");
}

#[test]
fn copy_json_of_an_unknown_atlas_prints_an_error_object_and_exits_2() {
    let (out, _s, _d) = with_env(&["leaddbs", "copy", "--atlas", "Nope", "--json"]);
    assert_eq!(out.code, 2);
    let text = out.json()["error"].as_str().unwrap_or("").to_owned();
    assert!(text.contains("unknown atlas name"), "{text}");
}
