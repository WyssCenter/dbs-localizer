#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use super::super::clinical::{ClinicalKind, ClinicalVar};
use super::super::exports::to_tsv;
use super::super::overlap::{SideVta, SubjectVta};
use super::super::record::GroupRecord;
use super::super::stats_common::{pearson, t_test_two_sample, Rng};
use super::*;

fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} vs {b}");
}

fn side(volume: f64, overlap: f64, efield: Option<f64>) -> SideVta {
    SideVta {
        amplitudes: vec![3.0],
        volume_mm3: Some(volume),
        atlas_volume: vec![Some(20.0)],
        intersection: vec![Some(overlap)],
        n_intersection: vec![Some(overlap / volume)],
        n_within: vec![Some(overlap / 20.0)],
        efield_volume: efield,
        efield_intersection: vec![efield.map(|e| e / 2.0)],
        efield_n_intersection: vec![efield.map(|_| 0.5)],
    }
}

fn subject(name: &str, right: Option<SideVta>, left: Option<SideVta>) -> SubjectVta {
    SubjectVta {
        subject: name.to_owned(),
        group: 1,
        group_label: "Group 1".to_owned(),
        label: "gs_1".to_owned(),
        structures: vec!["STN".to_owned()],
        sides: [right, left],
    }
}

fn record(names: &[&str]) -> GroupRecord {
    let mut r = GroupRecord::new("20261023101500", "ds");
    let d = r.ensure_dataset("/ds", "/ds/derivatives/leaddbs");
    for n in names {
        r.add_patient(n, &d, 1).unwrap();
    }
    r
}

fn var(name: &str, kind: ClinicalKind, values: Vec<Vec<Option<f64>>>) -> ClinicalVar {
    ClinicalVar {
        name: name.to_owned(),
        kind,
        values,
        measure: "other".to_owned(),
        higher_is_better: true,
        unit: String::new(),
        description: String::new(),
        source: String::new(),
    }
}

fn row<'a>(s: &'a ClinicalStats, side: &str) -> &'a CorrelationRow {
    s.correlations.iter().find(|r| r.side == side).unwrap()
}

#[test]
fn exact_p_of_a_perfect_correlation_of_three_points() {
    let r = exact_permutation_test(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
    assert!(r.exact);
    close(r.pearson_r, 1.0, 1e-12);
    close(r.pearson_p, 2.0 / 6.0, 1e-12);
    close(r.spearman_p, 2.0 / 6.0, 1e-12);
}

#[test]
fn exact_p_of_four_points_is_hand_computed() {
    let r = exact_permutation_test(&[1.0, 2.0, 3.0, 4.0], &[1.0, 3.0, 2.0, 4.0]);
    close(r.pearson_r, 0.8, 1e-12);
    close(r.spearman_r, 0.8, 1e-12);
    close(r.pearson_p, 8.0 / 24.0, 1e-12);
    close(r.spearman_p, 8.0 / 24.0, 1e-12);
}

#[test]
fn all_permutations_are_visited_once() {
    let mut seen: Vec<Vec<usize>> = Vec::new();
    for_each_permutation(4, |p| seen.push(p.to_vec()));
    assert_eq!(seen.len(), 24);
    assert_eq!(seen[0], vec![0, 1, 2, 3]);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 24);
}

#[test]
fn monte_carlo_p_matches_the_exact_p_within_tolerance() {
    let x = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
    let y = [2.0, 1.0, 4.0, 3.0, 7.0, 5.0, 6.0];
    let exact = exact_permutation_test(&x, &y);
    let mut rng = Rng::new(42);
    let mc = monte_carlo_permutation_test(&x, &y, 20_000, &mut rng);
    assert!(!mc.exact);
    close(mc.pearson_r, exact.pearson_r, 1e-12);
    close(mc.pearson_p, exact.pearson_p, 0.015);
    close(mc.spearman_p, exact.spearman_p, 0.015);
    assert!(mc.pearson_p > 0.0);
}

#[test]
fn permutation_test_switches_to_random_permutations_above_seven_points() {
    let x: Vec<f64> = (0..8).map(f64::from).collect();
    let y: Vec<f64> = x.iter().map(|v| v * v).collect();
    let mut rng = Rng::new(3);
    assert!(!permutation_test(&x, &y, 200, &mut rng).exact);
    assert!(permutation_test(&x[..7], &y[..7], 200, &mut rng).exact);
    assert!(permutation_test(&x[..2], &y[..2], 200, &mut rng)
        .pearson_p
        .is_nan());
}

#[test]
fn pooled_t_test_matches_textbook_values() {
    let t = t_test_two_sample(&[1.0, 2.0, 3.0, 4.0, 5.0], &[3.0, 4.0, 5.0, 6.0, 7.0]);
    close(t.stat, -2.0, 1e-12);
    close(t.p, 0.080_516, 1e-4);
    let t = t_test_two_sample(&[2.0, 4.0, 6.0], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    close(t.stat, -0.313_33, 1e-4);
}

#[test]
fn box_plot_numbers_have_whiskers_and_outliers() {
    let b = box_stats(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 100.0]);
    assert_eq!(b.n, 10);
    close(b.q1.unwrap(), 3.25, 1e-12);
    close(b.median.unwrap(), 5.5, 1e-12);
    close(b.q3.unwrap(), 7.75, 1e-12);
    assert_eq!(b.whisker_low, Some(1.0));
    assert_eq!(b.whisker_high, Some(9.0));
    assert_eq!(b.outliers, vec![100.0]);
    assert_eq!(box_stats(&[]).n, 0);
}

fn cohort() -> (GroupRecord, Vec<SubjectVta>) {
    let mut r = record(&["sub-01", "sub-02", "sub-03", "sub-04"]);
    r.clinical.push(var(
        "score",
        ClinicalKind::PerPatient,
        vec![
            vec![Some(1.0)],
            vec![Some(2.0)],
            vec![Some(4.0)],
            vec![Some(3.0)],
        ],
    ));
    r.clinical.push(var(
        "hemi",
        ClinicalKind::PerHemisphere,
        vec![
            vec![Some(1.0), Some(5.0)],
            vec![Some(2.0), Some(7.0)],
            vec![Some(4.0), Some(6.0)],
            vec![Some(3.0), None],
        ],
    ));
    r.clinical.push(var(
        "responder",
        ClinicalKind::PerPatient,
        vec![
            vec![Some(0.0)],
            vec![Some(0.0)],
            vec![Some(1.0)],
            vec![Some(1.0)],
        ],
    ));
    let subjects = vec![
        subject(
            "sub-01",
            Some(side(10.0, 1.0, None)),
            Some(side(10.0, 3.0, None)),
        ),
        subject(
            "sub-02",
            Some(side(10.0, 2.0, None)),
            Some(side(10.0, 1.0, None)),
        ),
        subject("sub-03", Some(side(10.0, 5.0, None)), None),
        subject(
            "sub-04",
            Some(side(10.0, 4.0, None)),
            Some(side(10.0, 2.0, None)),
        ),
    ];
    (r, subjects)
}

#[test]
fn per_patient_variable_uses_the_sum_of_both_sides_with_a_missing_side_as_zero() {
    let (r, subjects) = cohort();
    let s = clinical_stats(&r, &subjects, &ClinicalStatsOptions::new("score")).unwrap();
    let both = row(&s, "both");
    assert_eq!(both.n, 4);
    let expect = pearson(&[4.0, 3.0, 5.0, 6.0], &[1.0, 2.0, 4.0, 3.0]);
    close(both.pearson_r.unwrap(), expect, 1e-12);
    assert_eq!(row(&s, "right").n, 4);
    assert_eq!(row(&s, "left").n, 3);
    let exact = exact_permutation_test(&[4.0, 3.0, 5.0, 6.0], &[1.0, 2.0, 4.0, 3.0]);
    close(both.pearson_p_perm.unwrap(), exact.pearson_p, 1e-12);
}

#[test]
fn per_hemisphere_variable_pairs_each_side_with_its_column_and_stacks_both() {
    let (r, subjects) = cohort();
    let s = clinical_stats(&r, &subjects, &ClinicalStatsOptions::new("hemi")).unwrap();
    let right = row(&s, "right");
    let left = row(&s, "left");
    let both = row(&s, "both");
    assert_eq!((right.n, left.n, both.n), (4, 2, 6));
    close(
        right.pearson_r.unwrap(),
        pearson(&[1.0, 2.0, 5.0, 4.0], &[1.0, 2.0, 4.0, 3.0]),
        1e-12,
    );
    let stacked_x = [1.0, 2.0, 5.0, 4.0, 3.0, 1.0];
    let stacked_y = [1.0, 2.0, 4.0, 3.0, 5.0, 7.0];
    close(
        both.pearson_r.unwrap(),
        pearson(&stacked_x, &stacked_y),
        1e-12,
    );
    assert!(s.group_tests.is_empty());
}

#[test]
fn two_valued_per_patient_variable_gets_pooled_group_tests_with_box_plots() {
    let (r, subjects) = cohort();
    let mut options = ClinicalStatsOptions::new("responder");
    options.sides = vec!["right"];
    let s = clinical_stats(&r, &subjects, &options).unwrap();
    assert_eq!(s.group_tests.len(), 1);
    let g = &s.group_tests[0];
    assert_eq!(g.group_values, [0.0, 1.0]);
    assert_eq!((g.n1, g.n2), (2, 2));
    close(g.mean1.unwrap(), 1.5, 1e-12);
    close(g.mean2.unwrap(), 4.5, 1e-12);
    let t = t_test_two_sample(&[1.0, 2.0], &[5.0, 4.0]);
    close(g.t.unwrap(), t.stat, 1e-12);
    close(g.df.unwrap(), 2.0, 1e-12);
    close(g.p.unwrap(), t.p, 1e-12);
    assert_eq!(g.boxes[0].n, 2);
    let json = s.to_json();
    assert_eq!(json["group_tests"][0]["boxplot"][1]["median"], 4.5);
    assert!(json["group_tests"][0]["boxplot"][0]["outliers"].is_array());
    assert!(
        clinical_stats(&r, &subjects, &ClinicalStatsOptions::new("score"))
            .unwrap()
            .group_tests
            .is_empty()
    );
}

#[test]
fn e_field_measures_are_empty_without_an_e_field() {
    let (r, mut subjects) = cohort();
    let mut options = ClinicalStatsOptions::new("score");
    options.measure = "efield_in_structure".to_owned();
    options.sides = vec!["right"];
    let s = clinical_stats(&r, &subjects, &options).unwrap();
    assert_eq!(s.correlations[0].n, 0);
    assert_eq!(s.correlations[0].pearson_r, None);
    for (i, sub) in subjects.iter_mut().enumerate() {
        sub.sides[0] = Some(side(10.0, 1.0, Some(10.0 + i as f64)));
    }
    let s = clinical_stats(&r, &subjects, &options).unwrap();
    assert_eq!(s.correlations[0].n, 4);
    assert!(s.correlations[0].pearson_r.is_some());
}

#[test]
fn bad_input_is_refused() {
    let (r, subjects) = cohort();
    let mut o = ClinicalStatsOptions::new("score");
    o.measure = "volume".to_owned();
    assert!(clinical_stats(&r, &subjects, &o).is_err());
    assert!(clinical_stats(&r, &subjects, &ClinicalStatsOptions::new("none")).is_err());
    let err = clinical_stats(&r, &subjects[..2], &ClinicalStatsOptions::new("score")).unwrap_err();
    assert!(err.to_string().contains("E_GROUP_TOO_FEW"));
    let mut r = r;
    r.clinical.push(var(
        "contacts",
        ClinicalKind::PerContact,
        vec![vec![Some(1.0); 4]; 4],
    ));
    let err = clinical_stats(&r, &subjects, &ClinicalStatsOptions::new("contacts")).unwrap_err();
    assert!(err.to_string().contains("per contact"));
}

#[test]
fn results_do_not_depend_on_the_run_and_tables_have_the_spec_columns() {
    let names: Vec<String> = (1..=9).map(|i| format!("sub-{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut r = record(&refs);
    let values: Vec<Vec<Option<f64>>> = (0..9)
        .map(|i| vec![Some(f64::from([3, 1, 4, 1, 5, 9, 2, 6, 5][i]))])
        .collect();
    r.clinical
        .push(var("score", ClinicalKind::PerPatient, values));
    let subjects: Vec<SubjectVta> = (0..9)
        .map(|i| {
            subject(
                &format!("sub-{:02}", i + 1),
                Some(side(
                    10.0,
                    1.0 + f64::from([2, 7, 1, 8, 2, 8, 1, 8, 2][i]),
                    None,
                )),
                None,
            )
        })
        .collect();
    let mut o = ClinicalStatsOptions::new("score");
    o.permutations = 500;
    let a = clinical_stats(&r, &subjects, &o).unwrap();
    let b = clinical_stats(&r, &subjects, &o).unwrap();
    assert_eq!(a.correlations, b.correlations);
    let p = a.correlations[0].pearson_p_perm.unwrap();
    assert!(p > 0.0 && p <= 1.0);
    let tsv = to_tsv(&correlation_table(&a.correlations));
    assert_eq!(
        tsv.lines().next().unwrap(),
        "structure\tmeasure\tside\tvariable\tn\tpearson_r\tpearson_p_perm\tspearman_r\tspearman_p_perm"
    );
    assert_eq!(tsv.lines().count(), 1 + 3);
    assert!(tsv
        .lines()
        .nth(1)
        .unwrap()
        .starts_with("STN\toverlap_mm3\tboth\tscore\t9\t"));
    assert_eq!(
        to_tsv(&group_test_table(&[])).trim_end(),
        GROUP_TEST_COLUMNS.join("\t")
    );
}
