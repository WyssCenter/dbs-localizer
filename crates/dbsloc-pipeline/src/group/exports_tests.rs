#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use fastwarp_matfile::mat5::read_mat5_file;

use super::*;
use crate::group::overlap::{SideVta, SubjectVta};

const GUID: &str = "20261023101500";
const SPACE: &str = "MNI152NLin2009bAsym";

fn row(subject: &str, overlap: Option<f64>) -> VtaRow {
    VtaRow {
        subject: subject.to_owned(),
        group: 1,
        group_label: "STN, left".to_owned(),
        side: 'R',
        source: 1,
        structure: "STN".to_owned(),
        vta_volume_mm3: Some(10.0),
        overlap_mm3: overlap,
        frac_of_vta: Some(0.1 + 0.2),
        frac_of_structure: None,
        structure_volume_mm3: Some(20.0),
        efield_sum: None,
        efield_in_structure: None,
        frac_of_efield: None,
        amplitude: "3".to_owned(),
    }
}

#[test]
fn the_vta_table_uses_the_column_names_of_the_spec() {
    let table = vta_table(&[row("sub-01", Some(4.0))]);
    let tsv = to_tsv(&table);
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "subject\tgroup\tgroup_label\tside\tsource\tstructure\tvta_volume_mm3\toverlap_mm3\tfrac_of_vta\tfrac_of_structure\tstructure_volume_mm3\tefield_sum\tefield_in_structure\tfrac_of_efield\tamplitude"
    );
    assert_eq!(
        lines.next().unwrap(),
        "sub-01\t1\tSTN, left\tR\t1\tSTN\t10\t4\t0.30000000000000004\t\t20\t\t\t\t3"
    );
    assert!(tsv.ends_with('\n') && !tsv.contains('\r'));
}

#[test]
fn contact_and_summary_headers_follow_the_spec() {
    assert_eq!(
        to_tsv(&contact_table(&[])).trim_end(),
        "subject\tgroup\tgroup_label\tside\tcontact\tstructure\tdistance_mm\tinside_hull\tinside_voxel\tactive"
    );
    assert_eq!(
        to_tsv(&summary_table(&[])).trim_end(),
        "group\tgroup_label\tside\tstructure\tmeasure\tn\tmean\tsd\tmedian\tmin\tmax"
    );
}

#[test]
fn csv_quotes_fields_with_commas_and_quotes() {
    let mut r = row("sub-01", None);
    r.structure = "a\"b".to_owned();
    let csv = to_csv(&vta_table(&[r]));
    assert!(csv.contains("\"STN, left\""));
    assert!(csv.contains("\"a\"\"b\""));
    assert!(csv.contains("\r\n"));
}

#[test]
fn a_tab_in_a_text_cell_cannot_break_the_columns() {
    let mut r = row("sub-01", None);
    r.group_label = "a\tb".to_owned();
    let tsv = to_tsv(&vta_table(&[r]));
    assert_eq!(tsv.lines().nth(1).unwrap().split('\t').count(), 15);
}

fn side(volume: f64, overlap: f64) -> serde_json::Value {
    serde_json::json!({
        "amplitudes": [3.0, 0.0, 0.0, 0.0],
        "volume_mm3": volume,
        "atlas_volume_mm3": [20.0],
        "intersection_mm3": [overlap],
        "n_intersection": [overlap / volume],
        "n_within_intersection": [overlap / 20.0],
        "efield": {"volume": 0.0, "intersection": [0.0], "n_intersection": [0.0]},
    })
}

fn cohort(root: &Path) -> GroupRecord {
    let mut r = GroupRecord::new(GUID, "Cohort");
    let d = r.ensure_dataset(&root.to_string_lossy(), &root.to_string_lossy());
    for (s, v, o) in [("sub-01", 10.0, 4.0), ("sub-02", 8.0, 2.0)] {
        r.add_patient(s, &d, 1).unwrap();
        let dir = root
            .join(s)
            .join("stimulations")
            .join(SPACE)
            .join(format!("gs_{GUID}"));
        std::fs::create_dir_all(&dir).unwrap();
        let json = serde_json::json!({
            "schema": "dbsloc-vtastats",
            "label": format!("gs_{GUID}"),
            "atlas": {"structures": [{"name": "STN.nii.gz", "type": 3, "skipped": false}]},
            "sides": {"right": side(v, o), "left": side(v, o)},
        });
        std::fs::write(
            dir.join(format!("{s}_desc-vtastats.json")),
            serde_json::to_string(&json).unwrap(),
        )
        .unwrap();
    }
    r.add_patient("sub-03", &d, 1).unwrap();
    r
}

#[test]
fn the_export_writes_tables_sidecars_and_the_lead_dbs_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let folder = tmp.path().join("group");
    std::fs::create_dir_all(&folder).unwrap();
    let record = cohort(&data);
    let report = write_stats_exports(
        &record,
        &folder,
        &ExportOptions {
            csv: true,
            lead_dbs_mat: true,
            out_dir: None,
        },
    )
    .unwrap();
    assert_eq!(report.patients_with_vta, 2);
    assert_eq!(report.missing_vta, vec!["sub-03".to_owned()]);
    assert_eq!(report.vta_rows, 4);
    assert_eq!(report.summary_rows, 9);
    let exports = folder.join("exports");
    let stem = "dataset-Cohort_analysis-20261023101500";
    for name in [
        "atlasoverlap_stats.tsv",
        "atlasoverlap_stats.csv",
        "atlasoverlap_stats.json",
        "contactdistance_stats.tsv",
        "atlasoverlapsummary_stats.tsv",
    ] {
        assert!(
            exports.join(format!("{stem}_desc-{name}")).is_file(),
            "{name}"
        );
    }
    let tsv = std::fs::read_to_string(exports.join(format!("{stem}_desc-atlasoverlap_stats.tsv")))
        .unwrap();
    assert_eq!(tsv.lines().count(), 5);
    let sidecar: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(exports.join(format!("{stem}_desc-atlasoverlap_stats.json")))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["guid"], GUID);
    assert_eq!(sidecar["rows"], 4);
    assert_eq!(sidecar["inputs"].as_array().unwrap().len(), 3);
    assert!(sidecar["inputs"][0]["vtastats_sha256"].is_string());
    assert!(sidecar["inputs"][2]["vtastats_sha256"].is_null());

    let mat = read_mat5_file(&folder.join(format!("{stem}_desc-stats_export.mat"))).unwrap();
    let names: Vec<&str> = mat.iter().map(|(n, _)| n.as_str()).collect();
    for want in [
        "stimulation",
        "clinicalVars",
        "clinicalVarNames",
        "clinicalVarSelected",
        "atlas",
        "atlasNames",
        "AtlasIntersectionVAT",
        "nAtlasIntersectionVAT",
        "AtlasIntersectionEfield",
        "nAtlasIntersectionEfield",
    ] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    let intersection = &mat
        .iter()
        .find(|(n, _)| n == "AtlasIntersectionVAT")
        .unwrap()
        .1;
    assert_eq!(intersection.dims(), vec![3, 2]);
    let first = intersection.item(0).unwrap();
    assert!(matches!(first, Mat5Value::Double { data, .. } if data == &vec![4.0]));
    assert!(intersection.item(2).unwrap().is_empty_array());
    let efield = &mat
        .iter()
        .find(|(n, _)| n == "AtlasIntersectionEfield")
        .unwrap()
        .1;
    assert!(efield.item(0).unwrap().is_empty_array());
}

#[test]
fn an_output_folder_replaces_the_exports_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let record = cohort(&tmp.path().join("data"));
    let out = tmp.path().join("elsewhere");
    let report = write_stats_exports(
        &record,
        &tmp.path().join("group"),
        &ExportOptions {
            csv: false,
            lead_dbs_mat: false,
            out_dir: Some(out.clone()),
        },
    )
    .unwrap();
    assert!(report.files.iter().all(|f| f.starts_with(&out)));
    assert_eq!(report.files.len(), 6);
}

#[test]
fn a_missing_side_still_gives_an_empty_cell_not_an_error() {
    let vta = SubjectVta {
        subject: "sub-01".to_owned(),
        group: 1,
        group_label: "G".to_owned(),
        label: "gs_x".to_owned(),
        structures: vec!["STN".to_owned()],
        sides: [Some(SideVta::default()), None],
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut record = GroupRecord::new(GUID, "Cohort");
    let d = record.ensure_dataset(&tmp.path().to_string_lossy(), &tmp.path().to_string_lossy());
    record.add_patient("sub-01", &d, 1).unwrap();
    let vars = lead_dbs_export_variables(&record, &[vta]);
    let cells = &vars
        .iter()
        .find(|(n, _)| n == "AtlasIntersectionVAT")
        .unwrap()
        .1;
    assert!(cells.item(0).is_some_and(Mat5Value::is_empty_array));
    assert!(cells.item(1).is_some_and(Mat5Value::is_empty_array));
}

#[test]
fn fibre_counts_are_exported_only_when_a_patient_has_them() {
    use crate::group::ea_stats::{EaStats, FtStats, StimulationStats};
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let record = cohort(&data);
    assert!(fibre_count_variables(&record).is_empty());
    let stats = EaStats {
        stimulation: vec![StimulationStats {
            label: format!("gs_{GUID}"),
            ft: [
                Some(FtStats {
                    fibercounts: vec![1.0, 2.0],
                    nfibercounts: vec![0.1, 0.2],
                    labels: vec!["a".to_owned(), "b".to_owned()],
                }),
                None,
            ],
            ..StimulationStats::default()
        }],
        ..EaStats::default()
    };
    let bytes = mat5_bytes(&[("ea_stats".to_owned(), stats.to_mat())], true).unwrap();
    std::fs::write(stats_path(&data.join("sub-01"), "sub-01"), bytes).unwrap();
    let vars = fibre_count_variables(&record);
    let names: Vec<&str> = vars.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["fibercounts", "nfibercounts"]);
    let counts = &vars[0].1;
    assert_eq!(counts.dims(), vec![3, 2]);
    assert!(
        matches!(counts.item(0).unwrap(), Mat5Value::Double { data, .. } if data == &vec![1.0, 2.0])
    );
    assert!(counts.item(1).unwrap().is_empty_array());
    assert!(counts.item(3).unwrap().is_empty_array());
    assert!(
        matches!(vars[1].1.item(0).unwrap(), Mat5Value::Double { data, .. } if data == &vec![0.1, 0.2])
    );
}
