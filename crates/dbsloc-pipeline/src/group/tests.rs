#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use fastwarp_matfile::mat5::{mat5_bytes, read_mat5_file, Mat5Value};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::clinical::{import_table, ClinicalError, ClinicalKind, ClinicalVar, ImportOptions};
use super::ea_stats::{read_ea_stats, EaStats, EfieldStats, FtStats, StimulationStats, VatStats};
use super::lead_group_mat::{
    backup_foreign_twin_at, build_m, import_twin_edits, read_m, record_from_m, write_m_twin,
    write_m_twin_with, ReadEnv, SubjectData, TwinOptions,
};
use super::matio::to_json;
use super::record::{default_color, load_record, save_record, twin_path, GroupRecord, ViewState};
use super::{locate, GroupError};
use crate::vat::settings::{from_lead_dbs_s, to_lead_dbs_s, ModelKind, Stimulation};

const BOSTON: &str = "Boston Scientific Vercise Directed";
const GUID: &str = "20261023101500";

fn reco_json() -> Value {
    let side = |x: f64| {
        json!({
            "coords_mm": [[x, 0.0, 0.0]],
            "trajectory": [[x, 0.0, -5.0]],
            "markers": {
                "head": [x, 0.0, -8.0],
                "tail": [x, 0.0, 6.0],
                "x": [x + 0.65, 0.0, -8.0],
                "y": [x, 0.65, -8.0],
            },
            "trajvector": [0.0, 0.0, 1.0],
        })
    };
    json!({
        "version": 2,
        "subj_id": "01",
        "mni": {"1": side(12.0), "2": side(-12.0)},
        "props": {
            "1": {"elmodel": BOSTON, "manually_corrected": false},
            "2": {"elmodel": BOSTON, "manually_corrected": false},
        },
        "meta": {},
    })
}

fn stim(subject: &str) -> Stimulation {
    let mut s = Stimulation::blank(&format!("gs_{GUID}"), subject, ModelKind::Kuncel, [8, 8]);
    s.sides.right.sources[0].amplitude = 2.5;
    s
}

fn var(name: &str, kind: ClinicalKind, values: Vec<Vec<Option<f64>>>) -> ClinicalVar {
    let mut v = ClinicalVar::blank(name, kind, values.len(), values[0].len());
    v.values = values;
    v.source = "lead_group".to_owned();
    v
}

fn sample(root: &Path) -> (GroupRecord, Vec<SubjectData>, PathBuf) {
    let ds = root.join("ds");
    let deriv = ds.join("derivatives").join("leaddbs");
    for s in ["sub-01", "sub-02", "sub-03"] {
        std::fs::create_dir_all(deriv.join(s)).unwrap();
    }
    let folder = ds.join("derivatives").join("leadgroup").join(GUID);
    std::fs::create_dir_all(&folder).unwrap();
    let mut r = GroupRecord::new(GUID, "ds");
    let d = r.ensure_dataset(&ds.to_string_lossy(), &deriv.to_string_lossy());
    r.add_patient("sub-01", &d, 1).unwrap();
    r.add_patient("sub-02", &d, 2).unwrap();
    r.add_patient("sub-03", &d, 2).unwrap();
    r.groups[0].color = Some([0.1, 0.2, 0.3]);
    r.groups[1].color = Some([0.4, 0.5, 0.6]);
    r.clinical.push(var(
        "UPDRS",
        ClinicalKind::PerPatient,
        vec![vec![Some(1.5)], vec![None], vec![Some(-0.0)]],
    ));
    r.clinical.push(var(
        "Tremor",
        ClinicalKind::PerHemisphere,
        vec![
            vec![Some(1.0), Some(2.0)],
            vec![Some(3.0), None],
            vec![None, None],
        ],
    ));
    r.view = ViewState {
        list_select: vec![1, 3],
        clinical_list: vec![2],
        highlight_active: true,
        mirror_sides: true,
        normalisation: 2,
        colour_by: vec!["UPDRS".to_owned()],
        parcellation: Some("AAL".to_owned()),
        connectome: Some("Do not calculate connectivity stats".to_owned()),
        ..ViewState::default()
    };
    r.atlas_set = Some("DISTAL Minimal (Ewert 2017)".to_owned());
    if let Some(s) = r.stimulation.as_mut() {
        s.model = Some(ModelKind::Kuncel);
    }
    let lc = Mat5Value::struct_scalar(vec![
        ("graphmetric".to_owned(), Mat5Value::scalar(1.0)),
        ("name".to_owned(), Mat5Value::char("x")),
    ]);
    r.lead_group
        .passthrough
        .insert("ui.lc".to_owned(), to_json(&lc));
    r.lead_group.passthrough.insert(
        "ui.lastupdated".to_owned(),
        to_json(&Mat5Value::scalar(20_261_023_101_500.0)),
    );
    r.lead_group.passthrough.insert(
        "unknown_field".to_owned(),
        to_json(&Mat5Value::cell_row(vec![
            Mat5Value::char("a"),
            Mat5Value::row(&[1.0, f64::NAN, 3.0]),
        ])),
    );
    r.lead_group
        .passthrough
        .insert("groups.extra".to_owned(), to_json(&Mat5Value::scalar(7.0)));
    r.lead_group
        .passthrough
        .insert("patient.note".to_owned(), to_json(&Mat5Value::char("n")));
    let subjects = ["sub-01", "sub-02", "sub-03"]
        .iter()
        .map(|s| SubjectData {
            subject: (*s).to_owned(),
            reco: Some(reco_json()),
            stimulation: Some(stim(s.trim_start_matches("sub-"))),
            stats: None,
        })
        .collect();
    (r, subjects, folder)
}

fn set_field(v: &mut Mat5Value, name: &str, new: Mat5Value) {
    if let Mat5Value::Struct {
        fields, elements, ..
    } = v
    {
        let i = fields.iter().position(|f| f == name).unwrap();
        elements[0][i] = new;
    }
}

fn field_mut<'a>(v: &'a mut Mat5Value, name: &str) -> &'a mut Mat5Value {
    let Mat5Value::Struct {
        fields, elements, ..
    } = v
    else {
        panic!("not a struct");
    };
    let i = fields.iter().position(|f| f == name).unwrap();
    &mut elements[0][i]
}

fn edit_twin(path: &Path, f: impl FnOnce(&mut Mat5Value)) {
    let mut vars = read_mat5_file(path).unwrap();
    let mut f = Some(f);
    for (n, v) in &mut vars {
        if n == "M" {
            if let Some(f) = f.take() {
                f(v);
            }
        }
    }
    std::fs::write(path, mat5_bytes(&vars, true).unwrap()).unwrap();
}

#[test]
fn json_record_round_trip() {
    let tmp = TempDir::new().unwrap();
    let (mut r, _, folder) = sample(tmp.path());
    let path = super::record::record_path(&folder, "ds", GUID);
    save_record(&path, &mut r).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("}\n"));
    assert!(text.contains("\n  \"schema\": \"dbsloc-group/1\""));
    let back = load_record(&path).unwrap();
    assert_eq!(back, r);
}

#[test]
fn schema_and_guid_are_checked() {
    let mut r = GroupRecord::new(GUID, "ds");
    r.schema = "other/2".to_owned();
    assert!(r.validate().is_err());
    let r = GroupRecord::new("bad guid", "ds");
    assert!(r.validate().is_err());
}

#[test]
fn twin_written_by_us_reads_back_lossless() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    assert!(report.path.is_file());
    let read = read_m(&report.path).unwrap();
    let back = &read.record;
    assert_eq!(back.lead_group.mat_sha256, report.sha256);
    assert_eq!(back.guid, rec.guid);
    assert_eq!(back.dataset_name, "ds");
    assert_eq!(
        back.patients.iter().map(|p| &p.subject).collect::<Vec<_>>(),
        rec.patients.iter().map(|p| &p.subject).collect::<Vec<_>>()
    );
    assert_eq!(
        back.patients.iter().map(|p| p.group).collect::<Vec<_>>(),
        vec![1, 2, 2]
    );
    assert_eq!(back.datasets, rec.datasets);
    assert_eq!(back.groups.len(), 2);
    assert_eq!(back.groups[0].color, Some([0.1, 0.2, 0.3]));
    assert_eq!(back.groups[1].color, Some([0.4, 0.5, 0.6]));
    assert_eq!(back.clinical.len(), 2);
    for (a, b) in back.clinical.iter().zip(&rec.clinical) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.kind, b.kind);
        assert_eq!(a.values, b.values);
    }
    assert_eq!(
        back.clinical[0].values[0][0].map(f64::to_bits),
        Some(1.5f64.to_bits())
    );
    assert_eq!(
        back.clinical[0].values[2][0].map(f64::to_bits),
        Some((-0.0f64).to_bits())
    );
    assert!(back.clinical[1].values[1][1].is_none());
    assert_eq!(back.view, rec.view);
    assert_eq!(back.atlas_set, rec.atlas_set);
    assert_eq!(
        back.stimulation.as_ref().and_then(|s| s.model),
        Some(ModelKind::Kuncel)
    );
    assert_eq!(back.lead_group.passthrough, rec.lead_group.passthrough);
    assert!(back.lead_group.dropped_fields.is_empty());
    assert_eq!(read.stimulations.len(), 3);
    for (p, s) in read.stimulations.iter().enumerate() {
        let id = format!("0{}", p + 1);
        let (expected, _) = from_lead_dbs_s(&to_lead_dbs_s(&stim(&id)), &id, 8).unwrap();
        let got = s.as_ref().unwrap();
        assert_eq!(got.sides, expected.sides);
        assert_eq!(got.model, expected.model);
        assert_eq!(got.model_settings, expected.model_settings);
        assert_eq!(got.label, format!("gs_{GUID}"));
    }
}

#[test]
fn twin_has_the_documented_field_order_and_both_sides() {
    let tmp = TempDir::new().unwrap();
    let (rec, subjects, _) = sample(tmp.path());
    let (m, _) = build_m(&rec, &subjects, "root/", &TwinOptions::default());
    let Mat5Value::Struct { fields, .. } = &m else {
        panic!("M is not a struct");
    };
    let names: Vec<&str> = fields.iter().map(String::as_str).collect();
    assert_eq!(
        &names[..9],
        ["patient", "guid", "clinical", "ui", "S", "vatmodel", "root", "groups", "elstruct"]
    );
    assert_eq!(&names[9..11], ["isomatrix", "isomatrix_name"]);
    assert_eq!(names[11], "unknown_field");
    let el = m.field("elstruct", 0).unwrap();
    let Mat5Value::Struct { dims, elements, .. } = el else {
        panic!("elstruct");
    };
    assert_eq!(dims, &vec![1, 3]);
    let Mat5Value::Cell { dims, items } = &elements[0][2] else {
        panic!("coords_mm");
    };
    assert_eq!(dims, &vec![1, 2]);
    assert!(!items[0].is_empty_array());
    assert!(!items[1].is_empty_array());
}

#[test]
fn groups_are_renumbered_at_write() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, _) = sample(tmp.path());
    rec.set_group("sub-02", 3).unwrap();
    rec.set_group("sub-03", 3).unwrap();
    let (m, _) = build_m(&rec, &subjects, "root/", &TwinOptions::default());
    let patient = m.field("patient", 0).unwrap();
    let Mat5Value::Double { data, .. } = patient.field("group", 0).unwrap() else {
        panic!("group");
    };
    assert_eq!(data, &vec![1.0, 2.0, 2.0]);
    let groups = m.field("groups", 0).unwrap();
    let Mat5Value::Double { data, .. } = groups.field("group", 0).unwrap() else {
        panic!("groups.group");
    };
    assert_eq!(data, &vec![1.0, 2.0]);
}

#[test]
fn minimal_m_without_patient_group_warns() {
    let list = Mat5Value::Cell {
        dims: vec![2, 1],
        items: vec![
            Mat5Value::char("/nowhere/ds/derivatives/leaddbs/sub-01"),
            Mat5Value::char("/nowhere/ds/derivatives/leaddbs/sub-02"),
        ],
    };
    let m = Mat5Value::struct_scalar(vec![(
        "patient".to_owned(),
        Mat5Value::struct_scalar(vec![("list".to_owned(), list)]),
    )]);
    let out = record_from_m(
        &m,
        Path::new("dataset-ds_analysis-abc123.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap();
    assert!(out.warnings.iter().any(|n| n.code == Some("W_LG_GROUPS")));
    assert!(out.record.patients.iter().all(|p| p.group == 1));
    assert_eq!(out.record.guid, "abc123");
    assert_eq!(out.record.dataset_name, "ds");
    assert!(out.record.clinical.is_empty());
    assert_eq!(out.record.groups.len(), 1);
}

#[test]
fn non_struct_m_is_not_a_group() {
    let err = record_from_m(
        &Mat5Value::scalar(1.0),
        Path::new("x.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        GroupError::Lg {
            code: "E_LG_NOT_GROUP",
            ..
        }
    ));
    let no_list = Mat5Value::struct_scalar(vec![(
        "patient".to_owned(),
        Mat5Value::struct_scalar(vec![("group".to_owned(), Mat5Value::scalar(1.0))]),
    )]);
    let err = record_from_m(
        &no_list,
        Path::new("x.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        GroupError::Lg {
            code: "E_LG_NOT_GROUP",
            ..
        }
    ));
}

#[test]
fn wrong_row_count_clinical_variable_is_kept_in_passthrough() {
    let list = Mat5Value::Cell {
        dims: vec![1, 2],
        items: vec![
            Mat5Value::char("/n/ds/derivatives/leaddbs/sub-01"),
            Mat5Value::char("/n/ds/derivatives/leaddbs/sub-02"),
        ],
    };
    let clinical = Mat5Value::struct_scalar(vec![
        (
            "vars".to_owned(),
            Mat5Value::cell_row(vec![Mat5Value::Double {
                dims: vec![3, 1],
                data: vec![1.0, 2.0, 3.0],
            }]),
        ),
        (
            "labels".to_owned(),
            Mat5Value::cell_row(vec![Mat5Value::char("odd")]),
        ),
    ]);
    let m = Mat5Value::struct_scalar(vec![
        (
            "patient".to_owned(),
            Mat5Value::struct_scalar(vec![
                ("list".to_owned(), list),
                ("group".to_owned(), Mat5Value::row(&[1.0, 1.0])),
            ]),
        ),
        ("clinical".to_owned(), clinical),
    ]);
    let out = record_from_m(
        &m,
        Path::new("dataset-ds_analysis-abc.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap();
    assert!(out
        .warnings
        .iter()
        .any(|n| n.code == Some("W_LG_CLINICAL_ROWS")));
    assert!(out.record.clinical.is_empty());
    assert!(out
        .record
        .lead_group
        .passthrough
        .contains_key("clinical.vars[0]"));
}

#[test]
fn missing_patient_folders_are_remapped_to_the_leaddbs_derivatives() {
    let tmp = TempDir::new().unwrap();
    let ds = tmp.path().join("ds");
    let leaddbs = ds.join("derivatives").join("leaddbs");
    std::fs::create_dir_all(leaddbs.join("sub-01")).unwrap();
    let folder = ds.join("derivatives").join("leadgroup").join("abc123");
    let list = Mat5Value::Cell {
        dims: vec![2, 1],
        items: vec![
            Mat5Value::char("/elsewhere/ds/derivatives/leaddbs/sub-01"),
            Mat5Value::char("/elsewhere/ds/derivatives/leaddbs/sub-02"),
        ],
    };
    let m = Mat5Value::struct_scalar(vec![(
        "patient".to_owned(),
        Mat5Value::struct_scalar(vec![
            ("list".to_owned(), list),
            ("group".to_owned(), Mat5Value::row(&[1.0, 1.0])),
        ]),
    )]);
    let out = record_from_m(
        &m,
        &folder.join("dataset-ds_analysis-abc123.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap();
    let first = &out.record.patients[0];
    let root = out.record.derivatives_root_of(first).unwrap();
    assert_eq!(Path::new(root), leaddbs);
    let missing: Vec<_> = out
        .warnings
        .iter()
        .filter(|n| n.code == Some("W_LG_PATIENT_MISSING"))
        .collect();
    assert_eq!(missing.len(), 1);
    assert!(missing[0].text.contains("sub-02"));
}

#[test]
fn changed_twin_hash_imports_edits_made_in_lead_group() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    assert!(import_twin_edits(&mut rec, &folder)
        .unwrap()
        .changed
        .is_empty());
    edit_twin(&report.path, |m| {
        let patient = field_mut(m, "patient");
        set_field(
            patient,
            "group",
            Mat5Value::Double {
                dims: vec![3, 1],
                data: vec![1.0, 2.0, 3.0],
            },
        );
        let clinical = field_mut(m, "clinical");
        if let Mat5Value::Cell { dims, items } = field_mut(clinical, "vars") {
            items.push(Mat5Value::Double {
                dims: vec![3, 1],
                data: vec![5.0, f64::NAN, 7.0],
            });
            *dims = vec![1, items.len()];
        }
        if let Mat5Value::Cell { dims, items } = field_mut(clinical, "labels") {
            items.push(Mat5Value::char("NewScore"));
            *dims = vec![1, items.len()];
        }
    });
    let before = rec.lead_group.mat_sha256.clone();
    let imported = import_twin_edits(&mut rec, &folder).unwrap();
    assert_ne!(rec.lead_group.mat_sha256, before);
    assert!(imported.changed.contains(&"groups".to_owned()));
    assert!(imported.changed.contains(&"clinical".to_owned()));
    assert!(!imported.changed.contains(&"patients".to_owned()));
    assert_eq!(rec.patients[2].group, 3);
    assert_eq!(rec.clinical.len(), 3);
    assert_eq!(rec.clinical[2].name, "NewScore");
    assert_eq!(
        rec.clinical[2].values,
        vec![vec![Some(5.0)], vec![None], vec![Some(7.0)]]
    );
    assert_eq!(rec.clinical[0].source, "lead_group");
    assert_eq!(rec.patients[0].dataset, "d1");
    assert!(import_twin_edits(&mut rec, &folder)
        .unwrap()
        .changed
        .is_empty());
}

#[test]
fn foreign_twin_is_backed_up_before_it_is_overwritten() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    let twin = twin_path(&folder, "ds", GUID);
    std::fs::write(&twin, b"written by someone else").unwrap();
    let backup = backup_foreign_twin_at(&folder, &rec, "20261023111213")
        .unwrap()
        .unwrap();
    assert_eq!(
        backup.file_name().unwrap().to_string_lossy(),
        format!("dataset-ds_analysis-{GUID}_desc-backup20261023111213.mat")
    );
    assert_eq!(std::fs::read(&backup).unwrap(), b"written by someone else");
    std::fs::remove_file(&backup).unwrap();
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    let backups: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("_desc-backup"))
        .collect();
    assert_eq!(backups.len(), 1);
    let stamp = backups[0]
        .strip_prefix(&format!("dataset-ds_analysis-{GUID}_desc-backup"))
        .and_then(|r| r.strip_suffix(".mat"))
        .unwrap();
    assert_eq!(stamp.len(), 14);
    assert!(stamp.chars().all(|c| c.is_ascii_digit()));
    assert!(report
        .warnings
        .iter()
        .any(|n| n.text.contains("not written by this software")));
    assert!(backup_foreign_twin_at(&folder, &rec, "20261023111214")
        .unwrap()
        .is_none());
}

fn stats_for(label: &str) -> EaStats {
    EaStats {
        atlas_names: vec!["STN.nii.gz".to_owned(), "GPi.nii.gz".to_owned()],
        atlas_types: vec![3.0, 3.0],
        electrodes: None,
        patname: vec![["sub-01".to_owned(), "sub-01".to_owned()]],
        conmat: vec![[vec![vec![1.0, 2.0], vec![3.0, 4.0]], vec![vec![5.0, 6.0]]]],
        conmat_inside_hull: Vec::new(),
        conmat_inside_vox: Vec::new(),
        stimulation: vec![StimulationStats {
            label: label.to_owned(),
            vat: [
                vec![VatStats {
                    amp: vec![2.5, 0.0, 0.0, 0.0],
                    label: label.to_owned(),
                    contact: 1.0,
                    side: 1.0,
                    volume: 55.5,
                    atlas_intersection: vec![1.0, 0.0],
                    n_atlas_intersection: vec![0.5, 0.0],
                    n_within_atlas_intersection: vec![0.25, 0.0],
                    atlas_volume: vec![100.0, 200.0],
                }],
                vec![VatStats {
                    side: 2.0,
                    contact: 1.0,
                    ..VatStats::default()
                }],
            ],
            efield: [
                vec![EfieldStats {
                    volume: 9.0,
                    atlas_intersection: vec![4.0, 0.0],
                    n_atlas_intersection: vec![0.4, 0.0],
                }],
                Vec::new(),
            ],
            ft: [
                Some(FtStats {
                    fibercounts: vec![1.0, 2.0],
                    nfibercounts: vec![0.1, 0.2],
                    labels: vec!["a".to_owned(), "b".to_owned()],
                }),
                None,
            ],
        }],
    }
}

#[test]
fn ea_stats_file_reads_back_into_the_typed_struct() {
    let tmp = TempDir::new().unwrap();
    let stats = stats_for("gs_x");
    let path = tmp.path().join("sub-01_desc-stats.mat");
    let bytes = mat5_bytes(&[("ea_stats".to_owned(), stats.to_mat())], true).unwrap();
    std::fs::write(&path, bytes).unwrap();
    let read = read_ea_stats(&path).unwrap();
    assert!(read.unsupported.is_empty());
    assert_eq!(read.stats, stats);
    assert!(read.stats.stimulation_of("gs_x").is_some());
}

#[test]
fn legacy_stimulation_without_label_is_discarded() {
    let ea = Mat5Value::struct_scalar(vec![
        (
            "atlases".to_owned(),
            Mat5Value::struct_scalar(vec![
                (
                    "names".to_owned(),
                    Mat5Value::cell_row(vec![Mat5Value::char("A.nii")]),
                ),
                ("types".to_owned(), Mat5Value::row(&[3.0])),
                ("rebuild".to_owned(), Mat5Value::scalar(1.0)),
            ]),
        ),
        (
            "stimulation".to_owned(),
            Mat5Value::struct_scalar(vec![("vat".to_owned(), Mat5Value::scalar(1.0))]),
        ),
    ]);
    let mut notes = Vec::new();
    let stats = EaStats::from_mat(&ea, &mut notes).unwrap();
    assert!(stats.stimulation.is_empty());
    assert_eq!(stats.atlas_names, vec!["A.nii".to_owned()]);
    assert_eq!(notes.len(), 2);
}

#[test]
fn stats_over_the_size_limit_are_omitted() {
    let tmp = TempDir::new().unwrap();
    let (rec, mut subjects, _) = sample(tmp.path());
    subjects[0].stats = Some(stats_for(&format!("gs_{GUID}")));
    let (m, notes) = build_m(&rec, &subjects, "root/", &TwinOptions::default());
    assert!(m.field("stats", 0).is_some());
    assert!(m.field("vilist", 0).is_some());
    assert!(m.field("fclist", 0).is_some());
    assert!(notes.iter().all(|n| n.code != Some("W_LG_STATS_OMITTED")));
    let (m, notes) = build_m(&rec, &subjects, "root/", &TwinOptions { limit_bytes: 1 });
    assert!(m.field("stats", 0).is_none());
    assert!(m.field("vilist", 0).is_some());
    assert!(notes.iter().any(|n| n.code == Some("W_LG_STATS_OMITTED")));
    let mut rec = rec;
    let (_, _, folder) = sample(tmp.path());
    let report = write_m_twin_with(
        &mut rec,
        &subjects,
        &folder,
        &TwinOptions { limit_bytes: 1 },
    )
    .unwrap();
    assert!(report
        .warnings
        .iter()
        .any(|n| n.code == Some("W_LG_STATS_OMITTED")));
}

#[test]
fn unsafe_names_warn_that_lead_group_will_not_find_the_file() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    rec.dataset_name = "my-dataset".to_owned();
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    assert!(report.warnings.iter().any(|n| n.code == Some("W_LG_NAME")));
    assert!(report.path.is_file());
}

fn three_patients() -> GroupRecord {
    let mut r = GroupRecord::new(GUID, "ds");
    let d = r.ensure_dataset("/x/ds", "/x/ds/derivatives/leaddbs");
    for s in ["sub-01", "sub-02", "sub-03"] {
        r.add_patient(s, &d, 1).unwrap();
    }
    r
}

#[test]
fn clinical_import_handles_missing_values_and_unknown_subjects() {
    let mut r = three_patients();
    let table = "participant_id\tUPDRS\tscore_R\tscore_L\tk_R\n01\t12,5\t1\t2\t\nsub-02\tNA\t\t4\t7\nsub-99\t3\t3\t3\t3\n";
    let report = import_table(
        &mut r,
        table,
        '\t',
        &ImportOptions {
            source: "t.tsv".to_owned(),
            ..ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(report.unknown_subjects, vec!["sub-99".to_owned()]);
    assert_eq!(report.patients_without_row, vec!["sub-03".to_owned()]);
    assert_eq!(report.imported, vec!["UPDRS", "score", "k_R"]);
    let updrs = &r.clinical[0];
    assert_eq!(updrs.values, vec![vec![Some(12.5)], vec![None], vec![None]]);
    let score = &r.clinical[1];
    assert_eq!(score.kind, ClinicalKind::PerHemisphere);
    assert_eq!(
        score.values,
        vec![
            vec![Some(1.0), Some(2.0)],
            vec![None, Some(4.0)],
            vec![None, None]
        ]
    );
    assert_eq!(updrs.source, "tsv:t.tsv");
}

#[test]
fn clinical_import_with_no_matching_subject_is_an_error() {
    let mut r = three_patients();
    let err = import_table(
        &mut r,
        "participant_id\tx\nzz\t1\n",
        '\t',
        &ImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ClinicalError::NoSubjectMatches { .. }));
    assert!(r.clinical.is_empty());
}

#[test]
fn clinical_import_refuses_to_replace_without_the_flag() {
    let mut r = three_patients();
    let table = "participant_id,x\n01,1\n02,2\n03,3\n";
    import_table(&mut r, table, ',', &ImportOptions::default()).unwrap();
    r.clinical[0].unit = "pts".to_owned();
    let err = import_table(&mut r, table, ',', &ImportOptions::default()).unwrap_err();
    assert!(matches!(err, ClinicalError::Exists(_)));
    let report = import_table(
        &mut r,
        "participant_id,x\n01,9\n",
        ',',
        &ImportOptions {
            replace: true,
            ..ImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(report.replaced, vec!["x".to_owned()]);
    assert_eq!(r.clinical[0].values[0][0], Some(9.0));
    assert_eq!(r.clinical[0].unit, "pts");
}

#[test]
fn comma_decimals_are_accepted_only_with_a_tab_delimiter() {
    let mut r = three_patients();
    let err = import_table(
        &mut r,
        "participant_id,x\n01,\"1,5\"\n",
        ',',
        &ImportOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ClinicalError::Table(_)));
    let mut r = three_patients();
    import_table(
        &mut r,
        "\u{feff}participant_id\tx\n01\t1,5\n",
        '\t',
        &ImportOptions::default(),
    )
    .unwrap();
    assert_eq!(r.clinical[0].values[0][0], Some(1.5));
}

#[test]
fn clinical_export_has_headers_per_kind_and_empty_missing_cells() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, _, folder) = sample(tmp.path());
    rec.clinical.push(var(
        "Amp",
        ClinicalKind::PerContact,
        vec![
            vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
            vec![None, None, None, None],
            vec![None, None, None, None],
        ],
    ));
    let tsv = super::clinical::export_tsv(&rec);
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "participant_id\tgroup\tgroup_label\tUPDRS\tTremor_R\tTremor_L\tAmp_k1R\tAmp_k2R\tAmp_k1L\tAmp_k2L"
    );
    assert_eq!(
        lines.next().unwrap(),
        "sub-01\t1\tGroup 1\t1.5\t1\t2\t1\t2\t3\t4"
    );
    assert_eq!(lines.next().unwrap(), "sub-02\t2\tGroup 2\t\t3\t\t\t\t\t");
    let (tsv_path, sidecar) = super::clinical::write_clinical_export(&rec, &folder).unwrap();
    assert!(tsv_path.is_file());
    assert!(sidecar.is_file());
}

#[test]
fn removing_a_patient_removes_its_clinical_rows() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, _, _) = sample(tmp.path());
    assert!(rec.remove_patient("02"));
    assert_eq!(rec.patients.len(), 2);
    assert_eq!(rec.clinical[0].values.len(), 2);
    assert_eq!(rec.clinical[0].values[1], vec![Some(-0.0)]);
    assert!(!rec.remove_patient("sub-02"));
    assert!(rec.add_patient("sub-01", "d1", 1).is_err());
    rec.add_patient("sub-04", "d1", 1).unwrap();
    assert_eq!(rec.clinical[1].values[2], vec![None, None]);
    rec.validate().unwrap();
}

#[test]
fn default_colours_are_the_palette_with_halved_saturation() {
    let c = default_color(1);
    let max = c.iter().copied().fold(f64::MIN, f64::max);
    let min = c.iter().copied().fold(f64::MAX, f64::min);
    assert!((max - 232.0 / 255.0).abs() < 1e-9);
    assert!((min - 232.0 / 255.0 * (1.0 - 0.4375)).abs() < 1e-9);
    assert_eq!(default_color(37), default_color(1));
}

#[test]
fn locate_resolves_the_group_folder_and_dataset_name() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("MyCohort");
    let loc = locate(&root, GUID, None, None).unwrap();
    assert_eq!(
        loc.folder,
        root.join("derivatives").join("leadgroup").join(GUID)
    );
    assert_eq!(loc.dataset_name, "MyCohort");
    assert_eq!(
        loc.json_path().file_name().unwrap().to_string_lossy(),
        format!("dataset-MyCohort_analysis-{GUID}.json")
    );
    let again = locate(&loc.folder, "ignored1", None, None).unwrap();
    assert_eq!(again.guid, GUID);
    assert_eq!(again.dataset_name, "MyCohort");
    assert!(locate(&root, "bad-guid", None, None).is_err());
}

#[test]
fn open_group_imports_edits_and_reports_them() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    let json_path = super::record::record_path(&folder, "ds", GUID);
    save_record(&json_path, &mut rec).unwrap();
    let opened = super::open_group(&folder).unwrap();
    assert!(opened.notes.is_empty());
    edit_twin(&report.path, |m| {
        let groups = field_mut(m, "patient");
        set_field(
            groups,
            "group",
            Mat5Value::Double {
                dims: vec![3, 1],
                data: vec![2.0, 2.0, 2.0],
            },
        );
    });
    let opened = super::open_group(&json_path).unwrap();
    assert!(opened.notes.iter().any(|n| n
        .text
        .starts_with("Imported changes made in Lead Group: groups")));
    assert_eq!(opened.record.patients[0].group, 2);
    let reloaded = load_record(&json_path).unwrap();
    assert_eq!(reloaded.patients[0].group, 2);
}

#[test]
fn m_file_with_undecodable_fields_opens_and_lists_them() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, subjects, folder) = sample(tmp.path());
    let report = write_m_twin(&mut rec, &subjects, &folder).unwrap();
    let mut vars = read_mat5_file(&report.path).unwrap();
    for (n, v) in &mut vars {
        if n == "M" {
            let Mat5Value::Struct {
                fields, elements, ..
            } = v
            else {
                panic!("not a struct");
            };
            fields.push("customcalc".to_owned());
            elements[0].push(Mat5Value::Double {
                dims: vec![7, 9],
                data: vec![0.0; 63],
            });
            fields.push("notes".to_owned());
            elements[0].push(Mat5Value::CharMatrix {
                dims: vec![2, 3],
                rows: vec!["abc".into(), "def".into()],
            });
        }
    }
    let mut bytes = mat5_bytes(&vars, false).unwrap();
    let pattern = [5u8, 0, 0, 0, 8, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0];
    let at = bytes
        .windows(pattern.len())
        .position(|w| w == pattern)
        .unwrap();
    assert_eq!(bytes[at - 8], 6);
    bytes[at - 8] = 5;
    let path = tmp.path().join("odd_M.mat");
    std::fs::write(&path, bytes).unwrap();
    let read = read_m(&path).unwrap();
    assert!(read
        .record
        .lead_group
        .dropped_fields
        .contains(&"M.customcalc".to_owned()));
    assert!(read.record.lead_group.passthrough.contains_key("notes"));
    assert_eq!(read.record.patients.len(), 3);
}

fn stats_of(subject: &str, label: &str, atlases: &[&str]) -> EaStats {
    let mut s = stats_for(label);
    s.patname = vec![[subject.to_owned(), subject.to_owned()]];
    s.atlas_names = atlases.iter().map(|a| (*a).to_owned()).collect();
    s.atlas_types = vec![3.0; atlases.len()];
    s
}

fn read_back(rec: &GroupRecord, subjects: &[SubjectData]) -> super::lead_group_mat::MRead {
    let (m, _) = build_m(rec, subjects, "root/", &TwinOptions::default());
    record_from_m(
        &m,
        Path::new("dataset-ds_analysis-20261023101500.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap()
}

fn stale(read: &super::lead_group_mat::MRead) -> Option<String> {
    read.warnings
        .iter()
        .find(|n| n.code == Some("W_LG_STATS_STALE"))
        .map(|n| n.text.clone())
}

#[test]
fn matching_stats_give_no_stale_warning() {
    let tmp = TempDir::new().unwrap();
    let (rec, mut subjects, _) = sample(tmp.path());
    let label = format!("gs_{GUID}");
    for (i, s) in ["sub-01", "sub-02", "sub-03"].iter().enumerate() {
        subjects[i].stats = Some(stats_of(s, &label, &["STN.nii.gz", "GPi.nii.gz"]));
    }
    assert_eq!(stale(&read_back(&rec, &subjects)), None);
    let none = read_back(&rec, &sample(tmp.path()).1);
    assert_eq!(stale(&none), None);
}

#[test]
fn stats_of_another_patient_order_are_stale() {
    let tmp = TempDir::new().unwrap();
    let (rec, mut subjects, _) = sample(tmp.path());
    let label = format!("gs_{GUID}");
    subjects[0].stats = Some(stats_of("sub-02", &label, &["STN.nii.gz"]));
    let text = stale(&read_back(&rec, &subjects)).unwrap();
    assert!(text.contains("entry 1 is for sub-02"), "{text}");
}

#[test]
fn stats_with_different_atlas_structures_are_stale() {
    let tmp = TempDir::new().unwrap();
    let (rec, mut subjects, _) = sample(tmp.path());
    let label = format!("gs_{GUID}");
    subjects[0].stats = Some(stats_of("sub-01", &label, &["STN.nii.gz", "GPi.nii.gz"]));
    subjects[1].stats = Some(stats_of("sub-02", &label, &["STN.nii.gz"]));
    let text = stale(&read_back(&rec, &subjects)).unwrap();
    assert!(text.contains("atlas structures of sub-02 differ"), "{text}");
}

#[test]
fn stats_without_the_group_label_or_with_the_wrong_patient_count_are_stale() {
    let tmp = TempDir::new().unwrap();
    let (rec, mut subjects, _) = sample(tmp.path());
    let label = format!("gs_{GUID}");
    subjects[0].stats = Some(stats_of("sub-01", &label, &["STN.nii.gz"]));
    let (mut m, _) = build_m(&rec, &subjects, "root/", &TwinOptions::default());
    let Mat5Value::Struct { elements, .. } = field_mut(&mut m, "stats") else {
        panic!("stats is not a struct");
    };
    let Mat5Value::Struct {
        fields: ef,
        elements: ee,
        ..
    } = &mut elements[0][0]
    else {
        panic!("ea_stats is not a struct");
    };
    let stim = ef.iter().position(|f| f == "stimulation").unwrap();
    let Mat5Value::Struct {
        fields: sf,
        elements: se,
        ..
    } = &mut ee[0][stim]
    else {
        panic!("stimulation is not a struct");
    };
    let l = sf.iter().position(|f| f == "label").unwrap();
    se[0][l] = Mat5Value::char("gs_other");
    let read = record_from_m(
        &m,
        Path::new("dataset-ds_analysis-20261023101500.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap();
    let text = stale(&read).unwrap();
    assert!(
        text.contains("sub-01 has no entry for gs_20261023101500"),
        "{text}"
    );

    let (mut m, _) = build_m(&rec, &subjects, "root/", &TwinOptions::default());
    if let Mat5Value::Struct { dims, elements, .. } = field_mut(&mut m, "stats") {
        elements.pop();
        *dims = vec![1, 2];
    }
    let read = record_from_m(
        &m,
        Path::new("dataset-ds_analysis-20261023101500.mat"),
        Vec::new(),
        &ReadEnv::default(),
    )
    .unwrap();
    let text = stale(&read).unwrap();
    assert!(
        text.contains("it holds 2 patients, the group lists 3"),
        "{text}"
    );
}

#[test]
fn stale_stats_do_not_fail_reading_a_twin_file() {
    let tmp = TempDir::new().unwrap();
    let (mut rec, mut subjects, folder) = sample(tmp.path());
    let label = format!("gs_{GUID}");
    subjects[0].stats = Some(stats_of("sub-02", &label, &["STN.nii.gz"]));
    write_m_twin(&mut rec, &subjects, &folder).unwrap();
    let read = read_m(&twin_path(&folder, &rec.dataset_name, &rec.guid)).unwrap();
    assert!(stale(&read).is_some());
    assert_eq!(read.record.patients.len(), 3);
}
