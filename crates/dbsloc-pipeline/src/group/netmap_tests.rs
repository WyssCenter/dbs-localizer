#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::AtomicBool;

use super::fibrefilter::{AnalysisError, CvScheme};
use super::netmap::*;
use super::stats_common::{CorrKind, Rng};
use crate::connectome_maps::nii::diagonal_affine;
use crate::connectome_maps::no_progress;

const V: usize = 10;
const N: usize = 6;

fn outcome() -> Vec<f64> {
    vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0]
}

fn cohort(shuffle_signal: bool) -> NetmapData {
    cohort_with(shuffle_signal, &[4], 1.0)
}

fn cohort_with(shuffle_signal: bool, voxels: &[usize], noise: f32) -> NetmapData {
    let mut rng = Rng::new(5);
    let y = outcome();
    let mut rows = Vec::new();
    for p in 0..N {
        let mut row: Vec<f32> = (0..V)
            .map(|_| noise * rng.below(1000) as f32 / 1000.0)
            .collect();
        let signal = if shuffle_signal { y[(p + 2) % N] } else { y[p] };
        for v in voxels {
            row[*v] = signal as f32 / 60.0 + 0.001 * rng.below(10) as f32;
        }
        rows.push(row);
    }
    NetmapData {
        dims: [V, 1, 1],
        affine: diagonal_affine([1.0; 3], [0.0; 3]),
        rows,
        row_patient: (0..N).collect(),
        flipped: vec![false; N],
        n_patients: N,
        outcome: y,
        covariates: Vec::new(),
        group_of: vec![1; N],
        mask: vec![true; V],
    }
}

fn both_signs() -> NetmapSettings {
    NetmapSettings {
        show_negative: true,
        ..NetmapSettings::default()
    }
}

fn run(d: &NetmapData, s: &NetmapSettings, scheme: &CvScheme) -> NetmapResult {
    let cancel = AtomicBool::new(false);
    compute_netmap(d, s, scheme, "conn", "all", 1, &cancel, &no_progress).unwrap()
}

#[test]
fn rmap_peaks_at_planted_voxel() {
    let r = run(
        &cohort(false),
        &NetmapSettings::default(),
        &CvScheme::LeaveOneOut,
    );
    let best = (0..V)
        .max_by(|a, b| r.map.vals[*a].total_cmp(&r.map.vals[*b]))
        .unwrap();
    assert_eq!(best, 4);
    assert!(r.map.vals[4] > 0.99);
    assert!(r.coverage.iter().all(|c| *c == N as f32));
}

#[test]
fn similarity_prediction_recovers_ranking() {
    let wide = || cohort_with(false, &[2, 4, 6, 8], 0.05);
    let r = run(
        &wide(),
        &both_signs(),
        &CvScheme::LeaveNothingOut { permutations: 20 },
    );
    assert!(r.evaluation.r > 0.5, "lno r = {}", r.evaluation.r);
    assert!(r.p_perm.is_some());
    let loo = run(&wide(), &both_signs(), &CvScheme::LeaveOneOut);
    assert!(loo.evaluation.r > 0.2, "loo r = {}", loo.evaluation.r);
    let shuffled = run(&cohort(true), &both_signs(), &CvScheme::LeaveOneOut);
    assert!(shuffled.map.vals[4] < 0.99);
}

#[test]
fn other_maps_and_nan_handling() {
    let d = cohort(false);
    let cancel = AtomicBool::new(false);
    let mut rows: Vec<Vec<f32>> = d.rows.clone();
    rows[0][4] = f32::NAN;
    let refs: Vec<&[f32]> = rows.iter().map(Vec::as_slice).collect();
    let r = compute_map(&refs, &d.outcome, &NetmapSettings::default(), &cancel).unwrap();
    assert!(r.vals[4].is_finite());
    for kind in [MapKind::AMap, MapKind::CMap, MapKind::NMap, MapKind::TMap] {
        let s = NetmapSettings {
            map: kind,
            ..NetmapSettings::default()
        };
        let m = compute_map(&refs, &d.outcome, &s, &cancel).unwrap();
        assert_eq!(m.vals.len(), V);
    }
    let few: Vec<&[f32]> = refs[..2].to_vec();
    let m = compute_map(&few, &d.outcome[..2], &NetmapSettings::default(), &cancel).unwrap();
    assert!(m.vals.iter().all(|v| v.is_nan()));
}

#[test]
fn fisher_z_similarity_option() {
    let a = [0.1f32, 0.2, 0.3, 0.4, 0.5];
    let b = [0.1f32, 0.25, 0.3, 0.45, 0.5];
    let mask = [true; 5];
    let r = similarity(&a, &b, &mask, CorrKind::Pearson, false);
    let z = similarity(&a, &b, &mask, CorrKind::Pearson, true);
    assert!((z - r.atanh()).abs() < 1e-9);
    let nan = [f32::NAN; 5];
    assert!(similarity(&a, &nan, &mask, CorrKind::Pearson, true).is_nan());
}

#[test]
fn nmap_counts_only_shown_signs() {
    let cancel = AtomicBool::new(false);
    let rows = [vec![0.5f32], vec![0.5], vec![-0.5], vec![0.1]];
    let refs: Vec<&[f32]> = rows.iter().map(Vec::as_slice).collect();
    let y = [1.0; 4];
    let s = NetmapSettings {
        map: MapKind::NMap,
        ..NetmapSettings::default()
    };
    assert_eq!(compute_map(&refs, &y, &s, &cancel).unwrap().vals[0], 2.0);
    let both = NetmapSettings {
        show_negative: true,
        ..s
    };
    assert_eq!(compute_map(&refs, &y, &both, &cancel).unwrap().vals[0], 1.0);
}

#[test]
fn output_names_and_cancel() {
    assert_eq!(
        map_file_name("d", "g", "g", MapKind::RMap),
        "dataset-d_analysis-g_netmap-g_stat-rmap_map.nii.gz"
    );
    assert_eq!(MapKind::TMap.name(), "t");
    let d = cohort(false);
    let mut r = run(&d, &NetmapSettings::default(), &CvScheme::LeaveOneOut);
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let out = write_netmap_outputs(
        dir.path(),
        "d",
        "g",
        &mut r,
        d.dims,
        &d.affine,
        &serde_json::json!({}),
        &cancel,
    )
    .unwrap();
    let names: Vec<String> = out
        .files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"netmap.json".to_owned()));
    assert!(names.contains(&"dataset-d_analysis-g_netmap-g_stat-rmap_map.nii.gz".to_owned()));
    let cancel = AtomicBool::new(true);
    let dir2 = tempfile::tempdir().unwrap();
    let e = write_netmap_outputs(
        dir2.path(),
        "d",
        "g",
        &mut r,
        d.dims,
        &d.affine,
        &serde_json::json!({}),
        &cancel,
    );
    assert!(matches!(e, Err(AnalysisError::Cancelled)));
    let net = dir2.path().join("networkmapping");
    assert!(std::fs::read_dir(net).map_or(true, |mut it| it.next().is_none()));
}

#[test]
fn new_patient_prediction() {
    let d = cohort(false);
    let r = run(
        &d,
        &NetmapSettings::default(),
        &CvScheme::LeaveNothingOut { permutations: 0 },
    );
    let p = predict_new(&r.model, &r.map.vals, &d.mask, "conn", &d.rows[5]).unwrap();
    assert!(p.predicted.is_finite());
    assert!(predict_new(&r.model, &r.map.vals, &d.mask, "x", &d.rows[5]).is_err());
}
