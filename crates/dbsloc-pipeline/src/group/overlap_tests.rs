#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{json, Value};

use super::*;
use crate::group::ea_stats::EaStats;

const GUID: &str = "20261023101500";
const SPACE: &str = "MNI152NLin2009bAsym";

fn side_json(
    amps: [f64; 4],
    volume: f64,
    atlas_volume: f64,
    overlap: f64,
    n_int: f64,
    n_within: f64,
    efield: Option<(f64, f64, f64)>,
) -> Value {
    let (ev, ei, en) = efield.unwrap_or((0.0, 0.0, 0.0));
    json!({
        "amplitudes": amps,
        "volume_mm3": volume,
        "atlas_volume_mm3": [atlas_volume],
        "intersection_mm3": [overlap],
        "n_intersection": [n_int],
        "n_within_intersection": [n_within],
        "efield": {
            "volume": ev,
            "integral_v_per_m_mm3": 0.0,
            "intersection": [ei],
            "n_intersection": [en],
        },
    })
}

fn stats_json(right: Value, left: Value) -> Value {
    json!({
        "schema": "dbsloc-vtastats",
        "schema_version": 1,
        "label": format!("gs_{GUID}"),
        "model": "kuncel",
        "settings_sha256": "x",
        "atlas": {
            "id": "synth",
            "name": "Synth",
            "structures": [{"name": "STN.nii.gz", "type": 3, "skipped": false}],
        },
        "sides": {"right": right, "left": left},
    })
}

fn write_stats(root: &Path, subject: &str, value: &Value) {
    let dir = root
        .join(subject)
        .join("stimulations")
        .join(SPACE)
        .join(format!("gs_{GUID}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{subject}_desc-vtastats.json")),
        serde_json::to_string(value).unwrap(),
    )
    .unwrap();
}

fn record(root: &Path, subjects: &[&str]) -> GroupRecord {
    let mut r = GroupRecord::new(GUID, "Cohort");
    let d = r.ensure_dataset(&root.to_string_lossy(), &root.to_string_lossy());
    for s in subjects {
        r.add_patient(s, &d, 1).unwrap();
    }
    r
}

fn cohort() -> (tempfile::TempDir, GroupRecord) {
    let dir = tempfile::tempdir().unwrap();
    write_stats(
        dir.path(),
        "sub-01",
        &stats_json(
            side_json(
                [3.0, 0.0, 0.0, 0.0],
                10.0,
                20.0,
                4.0,
                0.4,
                0.2,
                Some((100.0, 40.0, 0.4)),
            ),
            side_json([2.5, 0.0, 0.0, 0.0], 8.0, 20.0, 2.0, 0.25, 0.1, None),
        ),
    );
    write_stats(
        dir.path(),
        "sub-02",
        &stats_json(
            side_json([2.0, 1.0, 0.0, 0.0], 6.0, 20.0, 3.0, 0.5, 0.15, None),
            side_json([0.0; 4], 0.0, 20.0, 0.0, 0.0, 0.0, None),
        ),
    );
    let r = record(dir.path(), &["sub-01", "sub-02", "sub-03"]);
    (dir, r)
}

fn close(a: Option<f64>, b: f64) {
    assert!(a.is_some_and(|a| (a - b).abs() < 1e-12), "{a:?} vs {b}");
}

#[test]
fn the_vta_table_has_one_row_per_patient_side_and_structure() {
    let (_dir, r) = cohort();
    let load = load_vta_stats(&r);
    assert_eq!(load.subjects.len(), 2);
    assert_eq!(load.missing, vec!["sub-03".to_owned()]);
    let rows = vta_rows(&load.subjects);
    assert_eq!(rows.len(), 4);
    let first = &rows[0];
    assert_eq!(
        (first.subject.as_str(), first.side, first.structure.as_str()),
        ("sub-01", 'R', "STN")
    );
    close(first.vta_volume_mm3, 10.0);
    close(first.overlap_mm3, 4.0);
    close(first.frac_of_vta, 0.4);
    close(first.frac_of_structure, 0.2);
    close(first.structure_volume_mm3, 20.0);
    close(first.efield_sum, 100.0);
    close(first.efield_in_structure, 40.0);
    close(first.frac_of_efield, 0.4);
    assert_eq!(first.amplitude, "3");
    assert_eq!(first.group_label, r.group_label_of(1));
    assert_eq!(rows[1].efield_sum, None);
    assert_eq!(rows[2].amplitude, "2;1");
    assert_eq!(rows[3].amplitude, "");
}

#[test]
fn summaries_match_the_hand_computed_statistics() {
    let (_dir, r) = cohort();
    let rows = summary_rows(&load_vta_stats(&r).subjects);
    let find = |side: &str, measure: &str| {
        rows.iter()
            .find(|x| x.side == side && x.measure == measure && x.structure == "STN")
            .unwrap()
            .clone()
    };
    let right = find("right", "overlap_mm3");
    assert_eq!(right.n, 2);
    close(right.mean, 3.5);
    close(right.sd, 0.5f64.sqrt());
    close(right.median, 3.5);
    close(right.min, 3.0);
    close(right.max, 4.0);
    let left = find("left", "overlap_mm3");
    assert_eq!(left.n, 1);
    close(left.mean, 2.0);
    assert_eq!(left.sd, None);
    let both = find("both", "overlap_mm3");
    assert_eq!(both.n, 2);
    close(both.mean, 4.5);
    close(find("both", "frac_of_vta").mean, 0.575);
    close(find("both", "frac_of_structure").mean, 0.225);
    assert_eq!(rows.len(), 9);
}

#[test]
fn groups_are_summarised_separately() {
    let (dir, mut r) = cohort();
    r.set_group("sub-02", 2).unwrap();
    let rows = summary_rows(&load_vta_stats(&r).subjects);
    let g2 = rows
        .iter()
        .find(|x| x.group == 2 && x.side == "right" && x.measure == "overlap_mm3")
        .unwrap();
    assert_eq!(g2.n, 1);
    close(g2.mean, 3.0);
    drop(dir);
}

#[test]
fn a_foreign_label_or_broken_file_leaves_the_patient_out() {
    let (dir, r) = cohort();
    let bad = dir
        .path()
        .join("sub-02/stimulations")
        .join(SPACE)
        .join(format!("gs_{GUID}/sub-02_desc-vtastats.json"));
    std::fs::write(&bad, "{").unwrap();
    let load = load_vta_stats(&r);
    assert_eq!(load.subjects.len(), 1);
    assert!(load.missing.contains(&"sub-02".to_owned()));
    assert_eq!(load.notes.len(), 1);
}

#[test]
fn describe_handles_even_counts_and_nan() {
    let [mean, sd, median, min, max] = describe(&[1.0, f64::NAN, 3.0, 5.0, 7.0]);
    close(mean, 4.0);
    close(median, 4.0);
    close(min, 1.0);
    close(max, 7.0);
    close(sd, (20.0f64 / 3.0).sqrt());
    assert_eq!(describe(&[]), [None; 5]);
}

#[test]
fn contact_rows_come_from_the_distance_matrices() {
    let patient = PatientEntry {
        subject: "sub-01".to_owned(),
        dataset: "d1".to_owned(),
        group: 1,
        elmodel_override: None,
        included: true,
    };
    let stats = EaStats {
        atlas_names: vec!["STN.nii.gz".to_owned(), "GPi.nii.gz".to_owned()],
        conmat: vec![[
            vec![vec![0.0, 2.5], vec![1.0, f64::NAN]],
            vec![vec![4.0, 5.0]],
        ]],
        conmat_inside_hull: vec![[
            vec![vec![0.0, 2.5], vec![1.0, f64::NAN]],
            vec![vec![4.0, 5.0]],
        ]],
        conmat_inside_vox: vec![[
            vec![vec![0.0, 2.5], vec![0.0, f64::NAN]],
            vec![vec![4.0, 5.0]],
        ]],
        ..EaStats::default()
    };
    let rows = contact_rows_of(&patient, "Group 1", &stats, None);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].structure, "STN");
    assert_eq!(rows[0].inside_hull, Some(true));
    assert_eq!(rows[1].inside_hull, Some(false));
    assert_eq!(rows[2].inside_voxel, Some(true));
    assert_eq!(rows[2].inside_hull, Some(false));
    assert_eq!(rows[2].contact, 2);
    assert_eq!(rows[3].side, 'L');
    assert_eq!(rows[3].active, None);
}
