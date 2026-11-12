// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::{Path, PathBuf};

use common::{dbsloc, run, s};

fn nifti(pixdim: [f32; 3]) -> Vec<u8> {
    let mut b = vec![0u8; 352 + 8];
    b[0..4].copy_from_slice(&348i32.to_le_bytes());
    b[40..42].copy_from_slice(&3i16.to_le_bytes());
    for i in 1..8 {
        let d: i16 = if i <= 3 { 2 } else { 1 };
        b[40 + 2 * i..42 + 2 * i].copy_from_slice(&d.to_le_bytes());
    }
    b[70..72].copy_from_slice(&2i16.to_le_bytes());
    b[72..74].copy_from_slice(&8i16.to_le_bytes());
    for (i, p) in pixdim.iter().enumerate() {
        b[80 + 4 * i..84 + 4 * i].copy_from_slice(&p.to_le_bytes());
    }
    b[108..112].copy_from_slice(&352.0f32.to_le_bytes());
    b[252..254].copy_from_slice(&1i16.to_le_bytes());
    b[344..348].copy_from_slice(b"n+1\0");
    b
}

fn write_nii(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    common::write(&p, &nifti([1.0, 1.0, 1.0]));
    p
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    src: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = tmp.path().join("ds");
    let src = tmp.path().join("Pat_07");
    std::fs::create_dir_all(&src).unwrap_or_else(|e| panic!("{e}"));
    Fixture {
        _tmp: tmp,
        root,
        src,
    }
}

fn import(args: &[&str]) -> common::Out {
    run(dbsloc().arg("import").args(args))
}

#[test]
fn explicit_items_import_loose_nifti_files() {
    let f = fixture();
    let t1 = write_nii(&f.src, "anat.nii");
    common::write(&f.src.join("anat.json"), br#"{"EchoTime": 0.003}"#);
    let ct = write_nii(&f.src, "post.nii");
    let root = s(&f.root);
    let out = import(&[
        &root,
        "--subject",
        "01",
        "--item",
        &format!("preop:T1w={}", s(&t1)),
        "--item",
        &format!("postop:ct={}", s(&ct)),
    ]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    let anat = f.root.join("rawdata/sub-01/ses-preop/anat");
    assert!(anat.join("sub-01_ses-preop_T1w.nii.gz").is_file());
    assert!(anat.join("sub-01_ses-preop_T1w.json").is_file());
    assert!(f
        .root
        .join("rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii.gz")
        .is_file());
    assert!(out.stderr.contains("dbsloc: import ["), "{}", out.stderr);
    let show = run(dbsloc().args(["subject", "show", &root, "01"]));
    assert_eq!(show.code, 0, "{}", show.stderr);
    assert!(show.stdout.contains("\"CT\""), "{}", show.stdout);

    let again = import(&[
        &root,
        "--subject",
        "01",
        "--item",
        &format!("preop:T1w={}", s(&t1)),
    ]);
    assert_eq!(again.code, 1, "{}", again.stderr);
    assert!(again.stderr.contains("E_IMPORT_RULES"), "{}", again.stderr);
    let replaced = import(&[
        &root,
        "--subject",
        "01",
        "--replace-existing",
        "--json",
        "--item",
        &format!("preop:T1w={}", s(&t1)),
    ]);
    assert_eq!(replaced.code, 0, "{}", replaced.stderr);
    let v = replaced.json();
    assert_eq!(v["subject_folder"], "sub-01");
    assert!(v["written"].as_array().is_some_and(|w| w.len() == 2));
}

#[test]
fn dry_run_json_prints_the_plan_and_writes_nothing() {
    let f = fixture();
    write_nii(&f.src, "t1_mprage.nii");
    write_nii(&f.src, "head_ct.nii");
    let out = import(&[
        &s(&f.root),
        "--nifti",
        &s(&f.src.join("t1_mprage.nii")),
        "--nifti",
        &s(&f.src.join("head_ct.nii")),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("removed"), "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["subject"], "Pat07");
    assert_eq!(v["subject_exists"], false);
    let files = v["files"].as_array().cloned().unwrap_or_default();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["suffix"], "T1w");
    assert_eq!(files[0]["session"], "preop");
    assert_eq!(
        files[0]["destination"],
        "rawdata/sub-Pat07/ses-preop/anat/sub-Pat07_ses-preop_T1w.nii.gz"
    );
    assert_eq!(files[1]["suffix"], "CT");
    assert_eq!(files[1]["session"], "postop");
    assert!(!f.root.exists());
}

#[test]
fn unassigned_images_stop_the_import_unless_auto() {
    let f = fixture();
    write_nii(&f.src, "t1_mprage.nii");
    write_nii(&f.src, "survey.nii");
    let root = s(&f.root);
    let t1 = s(&f.src.join("t1_mprage.nii"));
    let survey = s(&f.src.join("survey.nii"));
    let out = import(&[&root, "--subject", "02", "--nifti", &t1, "--nifti", &survey]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("survey.nii"), "{}", out.stderr);
    assert!(out.stderr.contains("--auto"), "{}", out.stderr);
    assert!(!f.root.exists());

    let auto = import(&[
        &root,
        "--subject",
        "02",
        "--nifti",
        &t1,
        "--nifti",
        &survey,
        "--auto",
    ]);
    assert_eq!(auto.code, 0, "{}", auto.stderr);
    assert!(
        auto.stderr.contains("left out (unassigned)"),
        "{}",
        auto.stderr
    );
    let anat = f.root.join("rawdata/sub-02/ses-preop/anat");
    assert!(anat.join("sub-02_ses-preop_T1w.nii.gz").is_file());
    assert_eq!(std::fs::read_dir(&anat).map(|r| r.count()).unwrap_or(0), 1);

    let assigned = import(&[
        &root,
        "--subject",
        "03",
        "--nifti",
        &t1,
        "--nifti",
        &survey,
        "--item",
        &format!("preop:T2w={survey}"),
        "--dry-run",
    ]);
    assert_eq!(assigned.code, 0, "{}", assigned.stderr);
    assert!(
        assigned.stdout.contains("sub-03_ses-preop_T2w.nii.gz"),
        "{}",
        assigned.stdout
    );
}

#[test]
fn usage_errors_exit_2() {
    let f = fixture();
    let root = s(&f.root);
    assert_eq!(import(&[&root]).code, 2);
    for bad in [
        "preop=x.nii",
        "preop:CT=x.nii",
        "intraop:T1w=x.nii",
        "preop:T1w:diag=x.nii",
    ] {
        let out = import(&[&root, "--subject", "01", "--item", bad]);
        assert_eq!(out.code, 2, "{bad}: {}", out.stderr);
    }
    let t1 = write_nii(&f.src, "t1.nii");
    let out = import(&[
        &root,
        "--subject",
        "bad_id",
        "--item",
        &format!("preop:T1w={}", s(&t1)),
    ]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("E_BIDS_PARSE"), "{}", out.stderr);
    let ct = write_nii(&f.src, "ct.nii");
    let out = import(&[
        &root,
        "--subject",
        "04",
        "--item",
        &format!("postop:CT={}", s(&ct)),
    ]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("E_NO_ANCHOR"), "{}", out.stderr);
    let not_dicom = import(&[&root, "--subject", "05", "--dicom", &s(&f.src)]);
    assert_eq!(not_dicom.code, 2, "{}", not_dicom.stderr);
}

#[test]
fn a_custom_lookup_table_from_the_environment_is_used() {
    let f = fixture();
    write_nii(&f.src, "anatomy.nii");
    let table = f.src.join("table.json");
    common::write(
        &table,
        br#"[{"session": "preop", "suffix": "T2w", "keywords": ["anatomy"]}]"#,
    );
    let out = run(dbsloc().env("DBSLOC_IMPORT_LOOKUP_JSON", s(&table)).args([
        "import",
        &s(&f.root),
        "--subject",
        "01",
        "--nifti",
        &s(&f.src.join("anatomy.nii")),
        "--dry-run",
        "--json",
    ]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["files"][0]["suffix"], "T2w");
    common::write(&table, b"not json");
    let out = run(dbsloc().env("DBSLOC_IMPORT_LOOKUP_JSON", s(&table)).args([
        "import",
        &s(&f.root),
        "--nifti",
        &s(&f.src.join("anatomy.nii")),
    ]));
    assert_eq!(out.code, 2, "{}", out.stderr);
}
