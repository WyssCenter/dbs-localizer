#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::AtomicBool;

use dbsloc_connectome::FibreSet;
use fastwarp_matfile::mat5::read_mat5_file;

use super::fibrefilter::*;
use super::stats_common::Rng;
use crate::connectome_maps::fibervals::{FeatureMatrix, FibreValues};
use crate::connectome_maps::no_progress;

fn matrix(total: usize, n_inputs: usize, rows: &[(u32, Vec<f32>)]) -> FeatureMatrix {
    let mut m = FeatureMatrix {
        total_fibers: total,
        n_inputs,
        connected: rows.iter().map(|r| r.0).collect(),
        ..FeatureMatrix::default()
    };
    for (_, v) in rows {
        for x in v {
            m.bin.push(f32::from(*x > 0.0));
            m.sum.push(*x);
            m.mean.push(*x);
            m.peak.push(*x);
            m.peak5.push(*x);
        }
    }
    m
}

fn data_of(m: FeatureMatrix, outcome: Vec<f64>) -> FibreData {
    let n = outcome.len();
    FibreData {
        n_patients: n,
        outcome: [outcome.clone(), outcome],
        per_hemisphere: false,
        covariates: Vec::new(),
        group_of: vec![1; n],
        sides: [Some(SideData::plain(m, (0..n).collect(), 0)), None],
    }
}

fn touch(patients: &[usize], n: usize, value: f32) -> Vec<f32> {
    (0..n)
        .map(|i| if patients.contains(&i) { value } else { 0.0 })
        .collect()
}

fn run(data: &FibreData, s: &FibreSettings, scheme: &CvScheme) -> AnalysisResult<FibreResult> {
    let cancel = AtomicBool::new(false);
    compute_fibrefilter(data, s, scheme, "conn", 1, &cancel, &no_progress)
}

fn vta_settings() -> FibreSettings {
    FibreSettings {
        mode: FibreMode::Vta,
        test: FibreTest::TwoSampleT,
        ..FibreSettings::default()
    }
}

fn vta_cohort() -> FibreData {
    let rows = vec![
        (3u32, touch(&[0, 1, 2, 3], 8, 1.0)),
        (5, touch(&[0, 4], 8, 1.0)),
        (7, touch(&[1, 2, 5, 6], 8, 1.0)),
        (11, touch(&[0, 1, 2, 3, 4, 5, 6, 7], 8, 1.0)),
    ];
    let outcome = vec![60.0, 62.0, 58.0, 61.0, 20.0, 22.0, 18.0, 21.0];
    data_of(matrix(20, 8, &rows), outcome)
}

fn small_set() -> FibreSet {
    let fibres: Vec<Vec<[f32; 3]>> = (0..20)
        .map(|f| (0..4).map(|k| [f as f32, k as f32, 0.0]).collect())
        .collect();
    FibreSet::from_fibres(&fibres).unwrap()
}

#[test]
fn vta_planted_fibre_has_top_positive_t() {
    let r = run(&vta_cohort(), &vta_settings(), &CvScheme::LeaveOneOut).unwrap();
    let s = r.scores[0].as_ref().unwrap();
    let best = (0..s.fibre.len())
        .max_by(|a, b| s.value[*a].total_cmp(&s.value[*b]))
        .unwrap();
    assert_eq!(s.fibre[best], 3);
    assert!(!s.fibre.contains(&19));
    assert!(!s.fibre.contains(&11));
    assert!(r.shown.iter().any(|x| x.fibre == 3 && x.value > 0.0));
}

#[test]
fn subset_file_has_expected_fibre_count_and_names() {
    let r = run(&vta_cohort(), &vta_settings(), &CvScheme::LeaveOneOut).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let out = write_fibrefilter_outputs(
        dir.path(),
        "g1",
        &r,
        &small_set(),
        "tag",
        &serde_json::json!({"x": 1}),
        &cancel,
    )
    .unwrap();
    assert_eq!(out.id, "g1");
    let names: Vec<String> = out
        .files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for want in ["fibrefilter.json", "fibres_R.mat", "fibrescores_R.tsv"] {
        assert!(names.contains(&want.to_owned()), "{names:?}");
    }
    assert!(out.dir.ends_with("fiberfiltering/g1") || out.dir.ends_with("fiberfiltering\\g1"));
    let vars = read_mat5_file(&out.dir.join("fibres_R.mat")).unwrap();
    let idx = vars.iter().find(|(k, _)| k == "idx").unwrap();
    assert_eq!(
        idx.1.dims()[0],
        r.shown.iter().filter(|s| s.side == 0).count()
    );
    let again = write_fibrefilter_outputs(
        dir.path(),
        "g1",
        &r,
        &small_set(),
        "tag",
        &serde_json::json!({}),
        &cancel,
    )
    .unwrap();
    assert_eq!(again.id, "g1_1");
}

#[test]
fn efield_linear_relation_gives_unit_correlation() {
    let n = 8;
    let values: Vec<f32> = (0..n).map(|i| 300.0 + 50.0 * i as f32).collect();
    let noise: Vec<f32> = vec![250.0, 0.0, 310.0, 0.0, 280.0, 0.0, 400.0, 0.0];
    let rows = vec![(3u32, values.clone()), (4, noise)];
    let outcome: Vec<f64> = values.iter().map(|v| 2.0 * f64::from(*v) + 5.0).collect();
    let s = FibreSettings {
        test: FibreTest::Pearson,
        ..FibreSettings::default()
    };
    let r = run(
        &data_of(matrix(10, n, &rows), outcome.clone()),
        &s,
        &CvScheme::LeaveOneOut,
    )
    .unwrap();
    let sc = r.scores[0].as_ref().unwrap();
    let i = sc.fibre.iter().position(|f| *f == 3).unwrap();
    assert!((sc.value[i] - 1.0).abs() < 1e-9);
    let s = FibreSettings {
        test: FibreTest::Spearman,
        ..FibreSettings::default()
    };
    let r = run(
        &data_of(matrix(10, n, &rows), outcome),
        &s,
        &CvScheme::LeaveOneOut,
    )
    .unwrap();
    let sc = r.scores[0].as_ref().unwrap();
    let i = sc.fibre.iter().position(|f| *f == 3).unwrap();
    assert!((sc.value[i] - 1.0).abs() < 1e-9);
}

fn planted(shuffle: bool) -> FibreData {
    let n = 16;
    let mut rng = Rng::new(11);
    let high: Vec<bool> = (0..n).map(|i| i % 2 == 0).collect();
    let mut rows: Vec<(u32, Vec<f32>)> = Vec::new();
    for f in 0..8u32 {
        let v: Vec<f32> = (0..n)
            .map(|i| {
                let on = match f {
                    2 => high[i],
                    6 => !high[i],
                    _ => rng.below(2) == 0,
                };
                if on {
                    250.0 + rng.below(100) as f32
                } else {
                    0.0
                }
            })
            .collect();
        rows.push((f, v));
    }
    let mut outcome: Vec<f64> = (0..n)
        .map(|i| if high[i] { 50.0 } else { 20.0 } + (rng.below(100) as f64) / 50.0)
        .collect();
    if shuffle {
        Rng::new(5).shuffle(&mut outcome);
    }
    data_of(matrix(10, n, &rows), outcome)
}

fn predictive_settings() -> FibreSettings {
    FibreSettings {
        basis: PredBasis::Sum,
        display: DisplaySettings {
            strategy: DisplayStrategy::FixedValue { value: 0.0 },
            show_positive: true,
            show_negative: true,
        },
        ..FibreSettings::default()
    }
}

#[test]
fn leave_one_out_predicts_planted_effect_and_not_shuffled() {
    let real = run(
        &planted(false),
        &predictive_settings(),
        &CvScheme::LeaveOneOut,
    )
    .unwrap();
    assert!(real.evaluation.r > 0.5, "r = {}", real.evaluation.r);
    let shuffled = run(
        &planted(true),
        &predictive_settings(),
        &CvScheme::LeaveOneOut,
    )
    .unwrap();
    assert!(shuffled.evaluation.r < 0.5, "r = {}", shuffled.evaluation.r);
    assert!(real.evaluation.r > shuffled.evaluation.r);
}

#[test]
fn other_cv_schemes_and_permutations_run() {
    let d = planted(false);
    let s = predictive_settings();
    let k = run(
        &d,
        &s,
        &CvScheme::KFold {
            k: 4,
            iterations: 2,
        },
    )
    .unwrap();
    assert!(k.evaluation.r > 0.3);
    let lno = run(&d, &s, &CvScheme::LeaveNothingOut { permutations: 30 }).unwrap();
    assert!(lno.p_perm.unwrap() < 0.2);
    assert!(lno.null95.is_some());
    let one_group = run(&d, &s, &CvScheme::LeaveOneCohortOut);
    assert!(one_group.is_err());
    let mut two = d.clone();
    two.group_of = (0..16).map(|i| 1 + (i % 2) as u32).collect();
    assert!(run(&two, &s, &CvScheme::LeaveOneCohortOut).is_ok());
}

#[test]
fn new_patient_prediction_uses_model_and_checks_connectome() {
    let d = planted(false);
    let r = run(
        &d,
        &predictive_settings(),
        &CvScheme::LeaveNothingOut { permutations: 0 },
    )
    .unwrap();
    let vals = FibreValues {
        total_fibers: 10,
        threshold: 200.0,
        fiber_index: vec![2],
        bin: vec![1],
        sum: vec![300.0],
        mean: vec![300.0],
        peak: vec![300.0],
        peak5: vec![300.0],
    };
    let p = predict_new(&r.model, "conn", &[Some(vals.clone()), None]).unwrap();
    assert!(p.ihat[0] > 0.0);
    assert!(p.predicted[0].is_finite());
    assert!(predict_new(&r.model, "other", &[Some(vals), None]).is_err());
}

#[test]
fn too_few_patients_and_cancel() {
    let rows = vec![(1u32, touch(&[0], 2, 1.0))];
    let d = data_of(matrix(5, 2, &rows), vec![1.0, 2.0]);
    let e = run(&d, &vta_settings(), &CvScheme::LeaveOneOut).unwrap_err();
    assert_eq!(e.code(), "E_GROUP_TOO_FEW");
    let cancel = AtomicBool::new(true);
    let r = compute_fibrefilter(
        &vta_cohort(),
        &vta_settings(),
        &CvScheme::LeaveOneOut,
        "c",
        1,
        &cancel,
        &no_progress,
    );
    assert!(matches!(r, Err(AnalysisError::Cancelled)));
}

#[test]
fn cancel_leaves_no_committed_file() {
    let r = run(&vta_cohort(), &vta_settings(), &CvScheme::LeaveOneOut).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(true);
    let e = write_fibrefilter_outputs(
        dir.path(),
        "g",
        &r,
        &small_set(),
        "t",
        &serde_json::json!({}),
        &cancel,
    );
    assert!(matches!(e, Err(AnalysisError::Cancelled)));
    let mut files = Vec::new();
    let mut stack = vec![dir.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                files.push(e.path());
            }
        }
    }
    assert!(files.is_empty(), "{files:?}");
}

#[test]
fn display_strategies_and_significance() {
    let d = planted(false);
    let mut s = predictive_settings();
    s.display.strategy = DisplayStrategy::FixedCount { count: 1 };
    let r = run(&d, &s, &CvScheme::LeaveOneOut).unwrap();
    assert!(r.shown.iter().filter(|x| x.value > 0.0).count() <= 1);
    s.significance = Some(super::stats_common::Correction::Bonferroni);
    s.alpha = 1e-12;
    s.display.strategy = DisplayStrategy::FixedValue { value: 0.0 };
    let r = run(&d, &s, &CvScheme::LeaveOneOut).unwrap();
    assert!(r.shown.is_empty());
}

mod patient_images_layout {
    use std::path::{Path, PathBuf};

    use fastwarp_core::Mat4;

    use super::super::fibrefilter::{patient_images, PatientImage};
    use super::super::flip::mirrored_affine;
    use super::super::Note;
    use crate::connectome_maps::job::{Side, SideFiles};
    use crate::connectome_maps::structural::SeedKind;
    use crate::vat::outputs::{write_binary_nii, Codes};

    const DIMS: [usize; 3] = [4, 2, 2];

    fn aff() -> Mat4 {
        [
            [1.0, 0.0, 0.0, 2.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn write(dir: &Path, name: &str, a: &Mat4) -> PathBuf {
        let path = dir.join(name);
        write_binary_nii(&path, DIMS, &[1; 16], a, Codes::default()).unwrap();
        path
    }

    fn original(dir: &Path, side: &str) -> PathBuf {
        write(
            dir,
            &format!("sub-01_sim-binary_model-kuncel_hemi-{side}.nii"),
            &aff(),
        )
    }

    fn mirror_name(side: &str, from: &str) -> String {
        format!("sub-01_sim-binary_model-kuncel_hemi-{side}_hemidesc-FlippedFrom{from}.nii")
    }

    fn mirror(dir: &Path, side: &str, from: &str, current: bool) -> PathBuf {
        let a = if current {
            mirrored_affine(DIMS, &aff())
        } else {
            aff()
        };
        write(dir, &mirror_name(side, from), &a)
    }

    fn image(column: usize, outcome_side: usize, flipped: bool, path: &Path) -> PatientImage {
        PatientImage {
            column,
            outcome_side,
            flipped,
            path: path.to_path_buf(),
        }
    }

    fn images(dir: &Path, files: Vec<(Side, PathBuf)>, notes: &mut Vec<Note>) -> Vec<PatientImage> {
        let found = SideFiles {
            model: "kuncel".to_owned(),
            files,
        };
        patient_images(dir, "01", SeedKind::Binary, &found, true, "sub-01", notes)
    }

    #[test]
    fn a_bilateral_patient_keeps_the_row_order_of_both_columns() {
        let dir = tempfile::tempdir().unwrap();
        let r = original(dir.path(), "R");
        let l = original(dir.path(), "L");
        let r_from_l = mirror(dir.path(), "R", "Left", true);
        let l_from_r = mirror(dir.path(), "L", "Right", true);
        let mut notes = Vec::new();
        let got = images(
            dir.path(),
            vec![(Side::Right, r.clone()), (Side::Left, l.clone())],
            &mut notes,
        );
        assert_eq!(
            got,
            vec![
                image(0, 0, false, &r),
                image(0, 1, true, &r_from_l),
                image(1, 1, false, &l),
                image(1, 0, true, &l_from_r),
            ]
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_left_only_patient_adds_its_mirror_to_the_right_column_with_the_left_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let l = original(dir.path(), "L");
        let r_from_l = mirror(dir.path(), "R", "Left", true);
        let mut notes = Vec::new();
        let got = images(dir.path(), vec![(Side::Left, l.clone())], &mut notes);
        assert_eq!(
            got,
            vec![image(0, 1, true, &r_from_l), image(1, 1, false, &l)]
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_right_only_patient_adds_its_mirror_to_the_left_column_with_the_right_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let r = original(dir.path(), "R");
        let l_from_r = mirror(dir.path(), "L", "Right", true);
        let mut notes = Vec::new();
        let got = images(dir.path(), vec![(Side::Right, r.clone())], &mut notes);
        assert_eq!(
            got,
            vec![image(0, 0, false, &r), image(1, 0, true, &l_from_r)]
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn without_mirroring_only_the_originals_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let r = original(dir.path(), "R");
        let l = original(dir.path(), "L");
        mirror(dir.path(), "R", "Left", true);
        mirror(dir.path(), "L", "Right", true);
        let found = SideFiles {
            model: "kuncel".to_owned(),
            files: vec![(Side::Right, r.clone()), (Side::Left, l.clone())],
        };
        let mut notes = Vec::new();
        let got = patient_images(
            dir.path(),
            "01",
            SeedKind::Binary,
            &found,
            false,
            "sub-01",
            &mut notes,
        );
        assert_eq!(got, vec![image(0, 0, false, &r), image(1, 1, false, &l)]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_missing_or_out_of_date_mirror_is_left_out_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let l = original(dir.path(), "L");
        for stale in [false, true] {
            if stale {
                mirror(dir.path(), "R", "Left", false);
            }
            let mut notes = Vec::new();
            let got = images(dir.path(), vec![(Side::Left, l.clone())], &mut notes);
            assert_eq!(got, vec![image(1, 1, false, &l)], "stale {stale}");
            assert_eq!(notes.len(), 1, "{notes:?}");
            let text = notes[0].to_string();
            assert!(text.contains(&mirror_name("R", "Left")), "{text}");
            assert!(text.contains("Calculate with mirroring again"), "{text}");
        }
    }
}
