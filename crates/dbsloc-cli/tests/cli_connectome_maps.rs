// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::{Path, PathBuf};

use common::{dbsloc, run, s};
use dbsloc_connectome::{write_data_mat, FibreSet};
use dbsloc_pipeline::connectome_maps::nii::{diagonal_affine, read_nii, write_nii};
use dbsloc_pipeline::connectome_maps::NiiVolume;

const SPACE: &str = "MNI152NLin2009bAsym";

fn line(y: f32, z: f32) -> Vec<[f32; 3]> {
    (-12..=12).map(|i| [i as f32 * 0.5, y, z]).collect()
}

fn connectomes(root: &Path) -> PathBuf {
    let c = root.join("connectomes");
    let folder = c.join("dMRI").join("Synth");
    std::fs::create_dir_all(&folder).unwrap_or_else(|e| panic!("{e}"));
    let mut f = vec![
        line(0.0, 0.0),
        line(1.0, 0.0),
        line(0.0, 0.2),
        line(4.0, 0.0),
    ];
    for i in 0..6 {
        f.push(line(20.0 + i as f32, 30.0));
    }
    let set = FibreSet::from_fibres(&f).unwrap_or_else(|e| panic!("{e}"));
    write_data_mat(&folder.join("data.mat"), &set).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        folder.join("dataset_info.json"),
        br#"{"name":"Synth","tag":"SYN"}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    c
}

fn stim_dir(dataset: &Path) -> PathBuf {
    dataset
        .join("derivatives")
        .join("leaddbs")
        .join("sub-01")
        .join("stimulations")
        .join(SPACE)
        .join("stim1")
}

fn write_image(dir: &Path, name: &str, centre: [f64; 3], value: f32) {
    let mut v = NiiVolume::zeros(
        [3, 3, 3],
        diagonal_affine(
            [1.0; 3],
            [centre[0] - 1.0, centre[1] - 1.0, centre[2] - 1.0],
        ),
    );
    v.data.fill(value);
    std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("{e}"));
    write_nii(&dir.join(name), &v).unwrap_or_else(|e| panic!("{e}"));
}

fn map_cmd(dataset: &Path, connectomes: &Path, extra: &[&str]) -> common::Out {
    run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(connectomes))
        .args(["connectome", "map", &s(dataset), "01", "--stim", "stim1"])
        .args(extra))
}

#[test]
fn map_writes_the_structural_files_for_a_subject() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let c = connectomes(d.path());
    let dataset = d.path().join("bids");
    write_image(
        &stim_dir(&dataset),
        "sub-01_sim-binary_model-kuncel_hemi-R.nii",
        [0.0, 0.0, 0.0],
        1.0,
    );
    let out = map_cmd(&dataset, &c, &["--connectome", "Synth", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["connectome"]["tag"], "SYN");
    assert_eq!(v["subjects"][0]["subject"], "01");
    assert_eq!(v["subjects"][0]["status"], "ok");
    let dir = stim_dir(&dataset);
    let map = read_nii(&dir.join("sub-01_sim-binary_model-kuncel_seed-dMRI_conn-SYN_strucmap.nii"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(map.data.iter().sum::<f32>(), 21.0);
    assert!(dir
        .join("sub-01_sim-binary_model-kuncel_seed-dMRI_conn-SYN_fibers.mat")
        .is_file());
    assert!(dir
        .join("sub-01_sim-binary_model-kuncel_seed-dMRI_conn-SYN_strucmap.json")
        .is_file());
}

#[test]
fn map_reports_skipped_with_exit_zero_and_errors_with_exit_one() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let c = connectomes(d.path());
    let dataset = d.path().join("bids");
    write_image(
        &stim_dir(&dataset),
        "sub-01_sim-binary_model-kuncel_hemi-R.nii",
        [30.0, 30.0, 10.0],
        1.0,
    );
    let out = map_cmd(&dataset, &c, &["--connectome", "Synth", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let status = out.json()["subjects"][0]["status"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    assert!(status.starts_with("skipped: "), "{status}");

    let out = run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(&c))
        .args(["connectome", "map", &s(&dataset), "02", "--stim", "stim1"])
        .args(["--connectome", "Synth", "--json"]));
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.json()["subjects"][0]["status"]
        .as_str()
        .unwrap_or("")
        .starts_with("error: E_SEED_EMPTY"));

    let out = map_cmd(&dataset, &c, &["--connectome", "Nope"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("E_CONN_MISSING"), "{}", out.stderr);
}

#[test]
fn usage_errors_exit_two() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let c = connectomes(d.path());
    let dataset = d.path().join("bids");
    let out = run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(&c))
        .args([
            "connectome",
            "map",
            &s(&dataset),
            "01",
            "02",
            "--stim",
            "stim1",
        ])
        .args(["--connectome", "Synth", "--seed", "x.nii"]));
    assert_eq!(out.code, 2, "{}", out.stderr);
    let out = map_cmd(&dataset, &c, &["--connectome", "Synth", "--grid", "3mm"]);
    assert_eq!(out.code, 2);
}

#[test]
fn fibervals_writes_one_file_per_available_side() {
    let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let c = connectomes(d.path());
    let dataset = d.path().join("bids");
    write_image(
        &stim_dir(&dataset),
        "sub-01_sim-efield_model-simbio_hemi-R.nii",
        [0.0, 0.0, 0.0],
        400.0,
    );
    let out = run(dbsloc()
        .env("DBSLOC_CONNECTOMES_DIR", s(&c))
        .args([
            "connectome",
            "fibervals",
            &s(&dataset),
            "01",
            "--stim",
            "stim1",
        ])
        .args(["--connectome", "Synth", "--threshold", "200", "--json"]));
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v = out.json();
    assert_eq!(v["subjects"][0]["status"], "ok");
    let dir = stim_dir(&dataset);
    assert!(dir
        .join("sub-01_sim-efield_model-simbio_hemi-R_conn-SYN_fibervals.mat")
        .is_file());
    assert!(!dir
        .join("sub-01_sim-efield_model-simbio_hemi-L_conn-SYN_fibervals.mat")
        .exists());
}
