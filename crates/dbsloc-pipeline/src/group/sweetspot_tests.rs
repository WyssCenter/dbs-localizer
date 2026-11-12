#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::clinical::{ClinicalKind, ClinicalVar};
use super::record::GroupRecord;
use super::stats_common as sc;
use super::sweetspot::*;
use crate::connectome_maps::nii::{diagonal_affine, encode_nii, NiiVolume};

fn ctl_run<T>(f: impl FnOnce(&Ctl<'_>) -> T) -> T {
    let cancel = AtomicBool::new(false);
    let progress = |_: &str| {};
    f(&Ctl {
        cancel: &cancel,
        progress: &progress,
    })
}

fn sphere_volume(centre: [f64; 3], radius: f64, side: usize, origin: f64) -> NiiVolume {
    let mut v = NiiVolume::zeros([side; 3], diagonal_affine([1.0; 3], [origin; 3]));
    for n in 0..v.len() {
        let c = v.centre(n);
        let d2: f64 = (0..3).map(|a| (c[a] - centre[a]).powi(2)).sum();
        if d2 <= radius * radius {
            v.data[n] = 1.0;
        }
    }
    v
}

struct Cohort {
    inputs: Vec<PatientInput>,
}

fn cohort(seed: u64, n: usize, planted: bool, with_flipped: bool) -> Cohort {
    let mut rng = sc::Rng::new(seed);
    let star = [0.0, 0.0, 0.0];
    let mut inputs = Vec::new();
    for p in 0..n {
        let centre = [
            rng.next_f64() * 12.0 - 6.0,
            rng.next_f64() * 12.0 - 6.0,
            rng.next_f64() * 12.0 - 6.0,
        ];
        let radius = 2.0 + 1.5 * rng.next_f64();
        let d: f64 = (0..3)
            .map(|a| (centre[a] - star[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        let hit = d <= radius + 2.0;
        let noise = 5.0 * rng.normal();
        let outcome = if planted {
            50.0 + if hit { 30.0 } else { 0.0 } + noise
        } else {
            50.0 + noise
        };
        let vol = sphere_volume(centre, radius, 24, -12.0);
        let img = FieldImage {
            volume: vol,
            binary: true,
        };
        inputs.push(PatientInput {
            subject: format!("sub-{p:02}"),
            group: 1 + (p % 2) as u32,
            fields: [Some(img.clone()), Some(img.clone())],
            flipped: if with_flipped {
                [Some(img.clone()), Some(img)]
            } else {
                [None, None]
            },
            amplitude: [2.0, 2.0],
            outcome: vec![outcome],
            covariates: Vec::new(),
        });
    }
    Cohort { inputs }
}

fn settings(resolution: f64) -> SweetspotSettings {
    SweetspotSettings {
        resolution,
        coverage_percent: 10.0,
        permutations: 1000,
        ..SweetspotSettings::default()
    }
}

fn tiny_matrix(rows: &[&[f32]], cut: f32) -> SideMatrix {
    let v = rows[0].len();
    SideMatrix {
        rows: rows.len(),
        v,
        data: rows.iter().flat_map(|r| r.iter().copied()).collect(),
        cut: vec![cut; rows.len()],
        empty: vec![false; rows.len()],
    }
}

#[test]
fn planted_sweet_spot_is_found() {
    let c = cohort(21, 40, true, false);
    let mut s = settings(1.0);
    let ds = build_dataset(&c.inputs, &s, false).unwrap();
    let side = &ds.sides[0];
    let q: Vec<usize> = (0..ds.n).collect();
    let zero = compute_map(
        &side.matrix,
        &side.coverage,
        &side.outcome,
        &side.amp,
        &q,
        &s,
        false,
    );
    assert!(zero.values.iter().any(|v| v.is_finite()));

    s.null_hypothesis = NullHypothesis::Average;
    let avg = compute_map(
        &side.matrix,
        &side.coverage,
        &side.outcome,
        &side.amp,
        &q,
        &s,
        false,
    );
    let (best, _) = avg
        .values
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_finite())
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let [nx, ny, _] = side.grid.dims;
    let ijk = [best % nx, (best / nx) % ny, best / (nx * ny)];
    let pos: Vec<f64> = (0..3)
        .map(|a| side.grid.origin[a] + ijk[a] as f64 * side.grid.resolution)
        .collect();
    let star = [0.0, 0.0, 0.0];
    let dist = (0..3)
        .map(|a| (pos[a] - star[a]).powi(2))
        .sum::<f64>()
        .sqrt();
    assert!(dist <= 1.5, "max t at {pos:?}, {dist} mm from S*");

    s.coverage_percent = 5.0;
    let ds = build_dataset(&c.inputs, &s, false).unwrap();
    s.cv = CvStrategy::LeaveOnePatientOut;
    let loo = ctl_run(|c| cross_validate(&ds, &s, c)).unwrap();
    assert!(loo.r > 0.5, "LOO r = {}", loo.r);

    s.cv = CvStrategy::LeaveNothingOut;
    let lno = ctl_run(|c| cross_validate(&ds, &s, c)).unwrap();
    assert!(
        lno.p_perm.unwrap() < 0.05,
        "p_perm = {:?}, r = {}",
        lno.p_perm,
        lno.r
    );
}

#[test]
fn null_cohort_is_not_significant() {
    let mut s = settings(2.0);
    s.null_hypothesis = NullHypothesis::Average;
    s.cv = CvStrategy::LeaveNothingOut;
    let mut ok = 0;
    for seed in 0..20u64 {
        let c = cohort(1000 + seed, 40, false, false);
        s.seed = seed + 1;
        let ds = build_dataset(&c.inputs, &s, false).unwrap();
        let cv = ctl_run(|ctl| cross_validate(&ds, &s, ctl)).unwrap();
        let p = cv.p_perm.unwrap();
        if p.is_nan() || p >= 0.05 {
            ok += 1;
        }
    }
    assert!(ok >= 18, "only {ok} of 20 seeds gave p_perm >= 0.05");
}

#[test]
fn hand_computed_voxel() {
    let m = tiny_matrix(&[&[1.0], &[1.0], &[1.0], &[0.0], &[0.0]], 0.5);
    let cov = Coverage::build(&m);
    let outcome = [10.0, 20.0, 30.0, 5.0, 7.0];
    let amp = [1.0; 5];
    let q: Vec<usize> = (0..5).collect();
    let mut s = settings(1.0);
    s.show_significant_only = false;
    s.coverage_percent = 20.0;
    let run = |s: &SweetspotSettings| compute_map(&m, &cov, &outcome, &amp, &q, s, false);

    s.test = Test::TTest;
    let t = run(&s).values[0];
    assert!((f64::from(t) - 20.0 / (10.0 / 3f64.sqrt())).abs() < 1e-5);
    assert!((f64::from(t) - 3.464).abs() < 1e-3);

    s.test = Test::MeanImage;
    assert_eq!(run(&s).values[0], 20.0);

    s.test = Test::NImage;
    assert_eq!(run(&s).values[0], 3.0);

    s.coverage_percent = 80.0;
    let r = run(&s);
    assert_eq!(r.n_c, 4);
    s.test = Test::TTest;
    assert!(run(&s).values[0].is_nan());
    s.test = Test::MeanImage;
    assert!(run(&s).values[0].is_nan());
}

#[test]
fn coverage_rounding_is_half_away_from_zero() {
    assert_eq!(coverage_count(10, 25.0), 3);
    assert_eq!(coverage_count(40, 20.0), 8);
    assert_eq!(coverage_count(5, 80.0), 4);
}

#[test]
fn mirroring_doubles_rows_and_loo_leaves_out_both() {
    let values: Vec<Vec<f64>> = (0..4).map(|p| vec![p as f64, 10.0 + p as f64]).collect();
    assert_eq!(
        stack_outcome(&values, 0, true),
        vec![0.0, 1.0, 2.0, 3.0, 10.0, 11.0, 12.0, 13.0]
    );
    assert_eq!(
        stack_outcome(&values, 1, true),
        vec![10.0, 11.0, 12.0, 13.0, 0.0, 1.0, 2.0, 3.0]
    );
    let single: Vec<Vec<f64>> = (0..3).map(|p| vec![p as f64]).collect();
    assert_eq!(
        stack_outcome(&single, 1, true),
        vec![0.0, 1.0, 2.0, 0.0, 1.0, 2.0]
    );

    let mut c = cohort(5, 6, true, true);
    for p in &mut c.inputs {
        p.outcome = vec![p.outcome[0], p.outcome[0] + 1.0];
    }
    let mut s = settings(2.0);
    s.mirror = true;
    let ds = build_dataset(&c.inputs, &s, true).unwrap();
    assert_eq!(ds.sides[0].matrix.rows, 12);
    assert_eq!(ds.sides[0].outcome.len(), 12);
    assert_eq!(ds.sides[1].outcome.len(), 12);
    assert_eq!(ds.sides[0].outcome[6], c.inputs[0].outcome[1]);
    let folds = make_folds(CvStrategy::LeaveOnePatientOut, ds.n, &ds.groups, 5, 1).unwrap();
    assert_eq!(folds.len(), 6);
    for (p, (train, test)) in folds.iter().enumerate() {
        assert_eq!(test, &vec![p]);
        let rows = ds.rows_of(train);
        assert_eq!(rows.len(), 10);
        assert!(!rows.contains(&p) && !rows.contains(&(p + 6)));
    }
}

#[test]
fn common_grid_extends_to_multiples_of_the_resolution() {
    let make = |lo: usize, hi: usize| {
        let mut v = NiiVolume::zeros([60, 2, 2], diagonal_affine([0.1; 3], [-11.0, 0.0, 0.0]));
        for k in 0..2 {
            for j in 0..2 {
                for i in lo..=hi {
                    let n = v.index(i, j, k);
                    v.data[n] = 200.0;
                }
            }
        }
        v
    };
    let a = make(8, 29);
    let b = make(20, 50);
    let g = common_grid(
        &[(&a, GRID_EFIELD_THRESHOLD), (&b, GRID_EFIELD_THRESHOLD)],
        0.5,
    )
    .unwrap();
    assert!((g.origin[0] + 10.5).abs() < 1e-9);
    assert_eq!(g.dims[0], 10);
    assert!((g.origin[0] + (g.dims[0] - 1) as f64 * 0.5 + 6.0).abs() < 1e-9);
}

#[test]
fn fdr_follows_the_reference_form() {
    let q = sc::fdr_reference(&[0.01, 0.04, 0.03]);
    for (a, b) in q.iter().zip([0.03, 0.04, 0.045]) {
        assert!((a - b).abs() < 1e-12);
    }
    let m = tiny_matrix(&[&[1.0, 0.0], &[1.0, 1.0], &[1.0, 1.0], &[0.0, 1.0]], 0.5);
    let cov = Coverage::build(&m);
    let mut s = settings(1.0);
    s.multiple_comparisons = Multiple::Bonferroni;
    s.coverage_percent = 0.0;
    let r = compute_map(
        &m,
        &cov,
        &[10.0, 11.0, 12.0, 13.0],
        &[1.0; 4],
        &[0, 1, 2, 3],
        &s,
        false,
    );
    assert_eq!(r.values.len(), 2);
}

#[test]
fn presets_set_the_table_fields() {
    let mut s = SweetspotSettings::default();
    s.apply_preset(Preset::Butson2011);
    assert_eq!(s.test, Test::MeanImage);
    assert_eq!(s.null_hypothesis, NullHypothesis::Threshold);
    assert_eq!(s.improvement_threshold, 0.5);
    s.apply_preset(Preset::Cheung2011);
    assert_eq!(s.test, Test::NImage);
    assert_eq!(s.improvement_threshold, 0.75);
    assert_eq!(s.n_threshold_percent, 75.0);
    s.apply_preset(Preset::Eisenstein2014);
    assert_eq!(
        (s.test, s.null_hypothesis),
        (Test::TTest, NullHypothesis::Zero)
    );
    assert_eq!(s.n_threshold_percent, 0.0);
    s.apply_preset(Preset::Reich2019);
    assert_eq!(
        (s.test, s.null_hypothesis),
        (Test::TTest, NullHypothesis::Average)
    );
    s.apply_preset(Preset::Dembek2019);
    assert_eq!(s.test, Test::Wilcoxon);
    assert_eq!(
        s.amplitude_correction,
        AmplitudeCorrection::AmplitudeDiscrete
    );
    s.apply_preset(Preset::Elias2020);
    assert_eq!(s.amplitude_correction, AmplitudeCorrection::VtaSize);
    assert_eq!(Preset::parse("Dembek 2019"), Some(Preset::Dembek2019));
    assert_eq!(Test::parse("Wilcoxon-Test"), Some(Test::Wilcoxon));
    assert_eq!(
        Test::parse("Proportion Test (Binary Var)"),
        Some(Test::Proportion)
    );
    assert_eq!(
        AmplitudeCorrection::parse("Amplitude (discrete)"),
        Some(AmplitudeCorrection::AmplitudeDiscrete)
    );
    assert_eq!(
        Normalisation::parse("van Albada 2007"),
        Some(Normalisation::VanAlbada)
    );
}

fn map_bits(a: &Analysis) -> Vec<u32> {
    a.maps
        .iter()
        .flatten()
        .flat_map(|m| m.values.iter().map(|v| v.to_bits()))
        .collect()
}

#[test]
fn analysis_is_deterministic() {
    let c = cohort(21, 20, true, false);
    let mut s = settings(2.0);
    s.permutations = 50;
    s.cv = CvStrategy::LeaveNothingOut;
    s.null_hypothesis = NullHypothesis::Average;
    let ds = build_dataset(&c.inputs, &s, false).unwrap();
    let a = ctl_run(|ctl| analyse(&ds, &s, ctl)).unwrap();
    let b = ctl_run(|ctl| analyse(&ds, &s, ctl)).unwrap();
    assert_eq!(map_bits(&a), map_bits(&b));
    assert_eq!(a.cv, b.cv);
    assert_eq!(a.model, b.model);
}

fn write_group(root: &Path, c: &Cohort, flipped: bool) -> GroupRecord {
    let deriv = root.join("derivatives").join("leaddbs");
    let mut record = GroupRecord::new("g1", "ds");
    let ds = record.ensure_dataset(&root.to_string_lossy(), &deriv.to_string_lossy());
    let mut var = ClinicalVar::blank("score", ClinicalKind::PerPatient, 0, 1);
    for (i, p) in c.inputs.iter().enumerate() {
        record
            .add_patient(&p.subject, &ds, 1 + (i % 2) as u32)
            .unwrap();
        var.values.push(vec![Some(p.outcome[0])]);
        let dir = deriv
            .join(&p.subject)
            .join("stimulations")
            .join("MNI152NLin2009bAsym")
            .join("gs_g1");
        std::fs::create_dir_all(&dir).unwrap();
        let img = p.fields[0].as_ref().unwrap();
        let sid = p.subject.trim_start_matches("sub-");
        for (side, from) in [("R", "Left"), ("L", "Right")] {
            let name = format!("sub-{sid}_sim-binary_model-simbio_hemi-{side}.nii");
            std::fs::write(dir.join(name), encode_nii(&img.volume)).unwrap();
            if flipped {
                let name = format!(
                    "sub-{sid}_sim-binary_model-simbio_hemi-{side}_hemidesc-FlippedFrom{from}.nii"
                );
                std::fs::write(dir.join(name), encode_nii(&img.volume)).unwrap();
            }
        }
    }
    record.clinical.push(var);
    record
}

fn run_on_disk(root: &Path, c: &Cohort, s: SweetspotSettings) -> (GroupRecord, SweetspotReport) {
    let record = write_group(root, c, s.mirror);
    let options = SweetspotOptions {
        group_folder: root.join("group"),
        variable: "score".to_owned(),
        covariates: Vec::new(),
        settings: s,
        space: "MNI152NLin2009bAsym".to_owned(),
        model: None,
        patients: None,
    };
    let cancel = AtomicBool::new(false);
    let report = run_sweetspot(&record, &options, &cancel, &|_| {}).unwrap();
    (record, report)
}

#[test]
fn run_writes_outputs_and_is_byte_identical() {
    let c = cohort(33, 14, true, false);
    let mut s = settings(2.0);
    s.permutations = 20;
    s.cv = CvStrategy::LeaveNothingOut;
    s.null_hypothesis = NullHypothesis::Average;
    s.split_by_group = true;
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let (_, r1) = run_on_disk(d1.path(), &c, s.clone());
    let (_, r2) = run_on_disk(d2.path(), &c, s.clone());
    assert_eq!(r1.id, "g1");
    assert!(r1.record_file.is_file() && r1.model_file.is_file());
    let names = |r: &SweetspotReport| -> Vec<String> {
        r.files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(names(&r1), names(&r2));
    assert!(names(&r1)
        .iter()
        .any(|n| n == "dataset-ds_analysis-g1_sweetspot-g1_hemi-R_stat-ttest_desc-pos_map.nii.gz"));
    assert!(names(&r1).iter().any(|n| n.contains("_group-2_hemi-L")));
    assert!(names(&r1)
        .iter()
        .any(|n| n.ends_with("_desc-coverage_map.nii.gz")));
    for (a, b) in r1.files.iter().zip(&r2.files) {
        assert_eq!(
            std::fs::read(a).unwrap(),
            std::fs::read(b).unwrap(),
            "{a:?}"
        );
    }
    let model = load_model(&r1.folder).unwrap();
    let hit = &c.inputs[0];
    let p = model.predict(&hit.fields);
    assert!(p.ihat[0].is_finite());
    let (_, r3) = run_on_disk(d1.path(), &c, s);
    assert_eq!(r3.id, "g1_1");
}

#[test]
fn efield_level_without_efields_is_refused() {
    let c = cohort(34, 8, true, false);
    let mut s = settings(2.0);
    s.level = Level::EFields;
    s.test = Test::Correlations;
    let d = tempfile::tempdir().unwrap();
    let record = write_group(d.path(), &c, false);
    let options = SweetspotOptions {
        group_folder: d.path().join("group"),
        variable: "score".to_owned(),
        covariates: Vec::new(),
        settings: s,
        space: "MNI152NLin2009bAsym".to_owned(),
        model: None,
        patients: None,
    };
    let cancel = AtomicBool::new(false);
    let err = run_sweetspot(&record, &options, &cancel, &|_| {}).unwrap_err();
    assert!(err.to_string().contains("E_SWEETSPOT_NO_EFIELD"), "{err}");
}

#[test]
fn too_few_patients_are_refused() {
    let c = cohort(35, 2, true, false);
    let s = settings(2.0);
    let d = tempfile::tempdir().unwrap();
    let record = write_group(d.path(), &c, false);
    let options = SweetspotOptions {
        group_folder: d.path().join("group"),
        variable: "score".to_owned(),
        covariates: Vec::new(),
        settings: s,
        space: "MNI152NLin2009bAsym".to_owned(),
        model: None,
        patients: None,
    };
    let cancel = AtomicBool::new(false);
    let err = run_sweetspot(&record, &options, &cancel, &|_| {}).unwrap_err();
    assert!(err.to_string().contains("E_GROUP_TOO_FEW"), "{err}");
}

#[test]
fn settings_validation_rejects_mismatched_level() {
    let mut s = SweetspotSettings {
        test: Test::Correlations,
        ..SweetspotSettings::default()
    };
    assert!(s.validate().is_err());
    s.level = Level::EFields;
    assert!(s.validate().is_ok());
    s.test = Test::TTest;
    assert!(s.validate().is_err());
}

#[test]
fn binomial_and_proportion_maps() {
    let m = tiny_matrix(&[&[1.0], &[1.0], &[1.0], &[1.0], &[0.0], &[0.0]], 0.5);
    let cov = Coverage::build(&m);
    let outcome = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
    let q: Vec<usize> = (0..6).collect();
    let mut s = settings(1.0);
    s.show_significant_only = false;
    s.coverage_percent = 0.0;
    s.test = Test::Binomial;
    let r = compute_map(&m, &cov, &outcome, &[1.0; 6], &q, &s, true);
    assert!((f64::from(r.values[0]) - (0.75 - 0.5)).abs() < 1e-6);
    let expected = sc::binomial_two_sided_p(3, 4, 0.5);
    assert!((f64::from(r.p[0]) - expected).abs() < 1e-6);
    s.test = Test::Proportion;
    let r = compute_map(&m, &cov, &outcome, &[1.0; 6], &q, &s, true);
    assert!(r.values[0] > 0.0);
}
