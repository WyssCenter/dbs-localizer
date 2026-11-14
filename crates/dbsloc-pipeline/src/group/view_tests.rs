#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use dbsloc_reconstruct::electrode_by_name;
use fastwarp_reg::transform::itk_h5::write_h5_file;
use fastwarp_reg::transform::{AffineTransform, TransformChain, TransformElem};

use super::super::fliplr::{forward_name, inverse_name};
use super::*;
use crate::vat::settings::{Contact, Mode as StimMode, ModelKind, Polarity};

const SPACE: &str = "MNI152NLin2009bAsym";
const MODEL: &str = "Medtronic 3389";
const GUID: &str = "20261023101500";

struct Fixture {
    _dir: tempfile::TempDir,
    folder: PathBuf,
    deriv: PathBuf,
    template: PathBuf,
    record: GroupRecord,
}

fn markers(head: [f64; 3]) -> Value {
    let model = electrode_by_name(MODEL).unwrap();
    let r = 0.5 * model.lead_diameter_mm;
    let tail = [head[0], head[1], head[2] + model.fiducial_span_mm()];
    json!({
        "head": head,
        "tail": tail,
        "x": [head[0] + r, head[1], head[2]],
        "y": [head[0], head[1] + r, head[2]],
    })
}

fn side(head: [f64; 3]) -> Value {
    let model = electrode_by_name(MODEL).unwrap();
    let contacts: Vec<[f64; 3]> = (0..model.n_contacts)
        .map(|i| [head[0], head[1], head[2] + 1.5 * i as f64])
        .collect();
    json!({
        "markers": markers(head),
        "coords_mm": contacts,
        "trajectory": [[head[0], head[1], head[2] - 5.0], [head[0], head[1], head[2] + 20.0]],
    })
}

fn write_reco(deriv: &Path, id: &str, right: [f64; 3], left: [f64; 3]) {
    let dir = deriv.join(format!("sub-{id}")).join("reconstruction");
    std::fs::create_dir_all(&dir).unwrap();
    let reco = json!({
        "version": 2,
        "subj_id": id,
        "props": {"1": {"elmodel": MODEL}, "2": {"elmodel": MODEL}},
        "meta": {"side_keys": "1=right,2=left", "template_space": SPACE},
        "mni": {"1": side(right), "2": side(left)},
    });
    std::fs::write(
        dir.join(format!("sub-{id}_desc-reconstruction.json")),
        serde_json::to_string_pretty(&reco).unwrap(),
    )
    .unwrap();
}

fn write_stimulation(deriv: &Path, id: &str, active: usize) {
    let folder = deriv.join(format!("sub-{id}"));
    let mut s = Stimulation::blank(&format!("gs_{GUID}"), id, ModelKind::Kuncel, [4, 4]);
    let src = &mut s.sides.get_mut(Side::Right).sources[0];
    src.mode = StimMode::Voltage;
    src.amplitude = 3.0;
    src.contacts[active] = Contact {
        index: active as u32 + 1,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 1.0,
    };
    crate::vat::settings::save(&folder, &s).unwrap();
}

fn fixture(name: &str, ids: &[&str]) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(&format!("dbsloc_view_{name}_"))
        .tempdir()
        .unwrap();
    let deriv = dir.path().join("derivatives");
    let template = dir.path().join("template").join("t1.nii.gz");
    std::fs::create_dir_all(template.parent().unwrap()).unwrap();
    std::fs::write(&template, b"x").unwrap();
    std::fs::write(
        template.parent().unwrap().join("template.json"),
        format!(r#"{{"space": "{SPACE}"}}"#),
    )
    .unwrap();
    let mut record = GroupRecord::new(GUID, "Cohort");
    let d = record.ensure_dataset(&dir.path().to_string_lossy(), &deriv.to_string_lossy());
    for (n, id) in ids.iter().enumerate() {
        let dx = 2.0 * n as f64;
        write_reco(
            &deriv,
            id,
            [10.0 + dx, -12.0, -8.0],
            [-10.0 - dx, -12.0, -8.0],
        );
        write_stimulation(&deriv, id, 1);
        record
            .add_patient(&format!("sub-{id}"), &d, 1 + (n % 2) as u32)
            .unwrap();
    }
    let folder = dir.path().join("group");
    std::fs::create_dir_all(&folder).unwrap();
    Fixture {
        _dir: dir,
        folder,
        deriv,
        template,
        record,
    }
}

fn options(fx: &Fixture) -> ViewOptions {
    let mut o = ViewOptions::new();
    o.template = Some(fx.template.to_string_lossy().into_owned());
    o.env.user_root = Some(fx.folder.join("userdata"));
    o.env.shipped_root = None;
    o
}

fn read_manifest(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn var(rows: Vec<Vec<Option<f64>>>, kind: ClinicalKind) -> ClinicalVar {
    let mut v = ClinicalVar::blank("score", kind, 0, 1);
    v.values = rows;
    v
}

use super::super::ClinicalKind;

fn row(v: &[f64]) -> Vec<Option<f64>> {
    v.iter().map(|x| Some(*x)).collect()
}

fn nan_eq(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x.is_nan() && y.is_nan()) || (x - y).abs() < 1e-12)
}

#[test]
fn one_column_gives_every_contact_the_patient_value() {
    let v = var(vec![row(&[5.0]), row(&[7.0])], ClinicalKind::PerPatient);
    let out = contact_values(&v, &[[4, 4], [3, 0]], Normalisation::None).unwrap();
    assert_eq!(out[0].kind, ValueKind::Contact);
    assert_eq!(out[0].sides[0], vec![5.0; 4]);
    assert_eq!(out[0].sides[1], vec![5.0; 4]);
    assert_eq!(out[1].sides[0], vec![7.0; 3]);
    assert!(out[1].sides[1].is_empty());
}

#[test]
fn two_columns_split_right_and_left() {
    let v = var(vec![row(&[1.0, 2.0])], ClinicalKind::PerHemisphere);
    let out = contact_values(&v, &[[4, 4]], Normalisation::None).unwrap();
    assert_eq!(out[0].sides[0], vec![1.0; 4]);
    assert_eq!(out[0].sides[1], vec![2.0; 4]);
}

#[test]
fn contact_pairs_are_drawn_at_midpoints_and_beyond_the_contact_count_nan() {
    let v = var(
        vec![
            row(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
            row(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
        ],
        ClinicalKind::PerContactPair,
    );
    let out = contact_values(&v, &[[4, 4], [3, 3]], Normalisation::None).unwrap();
    assert_eq!(out[0].kind, ValueKind::Pair);
    assert_eq!(out[0].sides[0], vec![1.0, 2.0, 3.0]);
    assert_eq!(out[0].sides[1], vec![4.0, 5.0, 6.0]);
    assert_eq!(out[1].sides[0], vec![1.0, 2.0]);
    assert_eq!(out[1].sides[1], vec![4.0, 5.0]);
}

#[test]
fn per_contact_columns_use_the_first_half_for_the_right_side() {
    let v = var(
        vec![
            row(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
            row(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]),
        ],
        ClinicalKind::PerContact,
    );
    let out = contact_values(&v, &[[4, 4], [2, 3]], Normalisation::None).unwrap();
    assert_eq!(out[0].kind, ValueKind::Contact);
    assert_eq!(out[0].sides[0], vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(out[0].sides[1], vec![5.0, 6.0, 7.0, 8.0]);
    assert_eq!(out[1].sides[0], vec![1.0, 2.0]);
    assert_eq!(out[1].sides[1], vec![5.0, 6.0, 7.0]);
}

#[test]
fn missing_values_stay_nan() {
    let mut v = var(vec![vec![None, Some(2.0)]], ClinicalKind::PerHemisphere);
    v.values[0][0] = None;
    let out = contact_values(&v, &[[2, 2]], Normalisation::None).unwrap();
    assert!(out[0].sides[0].iter().all(|x| x.is_nan()));
    assert_eq!(out[0].sides[1], vec![2.0, 2.0]);
}

#[test]
fn another_width_is_an_isomatrix_error() {
    let v = var(
        vec![row(&[1.0, 2.0, 3.0, 4.0, 5.0])],
        ClinicalKind::PerContact,
    );
    let err = contact_values(&v, &[[4, 4]], Normalisation::None).unwrap_err();
    match err {
        GroupError::Lg { code, .. } => assert_eq!(code, "E_GROUP_ISOMATRIX"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn z_score_is_applied_per_side_over_all_finite_values() {
    let v = var(
        vec![row(&[1.0, 10.0]), row(&[3.0, 20.0]), row(&[5.0, 30.0])],
        ClinicalKind::PerHemisphere,
    );
    let out = contact_values(&v, &[[2, 2], [2, 2], [2, 2]], Normalisation::ZScore).unwrap();
    let expected = zscore(&[1.0, 1.0, 3.0, 3.0, 5.0, 5.0]);
    assert!(nan_eq(&out[0].sides[0], &expected[0..2]));
    assert!(nan_eq(&out[1].sides[0], &[0.0, 0.0]));
    assert!(nan_eq(&out[2].sides[0], &expected[4..6]));
    let mean: f64 = out.iter().flat_map(|p| p.sides[1].iter()).sum::<f64>() / 6.0;
    assert!(mean.abs() < 1e-12);
    let ranks = contact_values(&v, &[[1, 1], [1, 1], [1, 1]], Normalisation::VanAlbada).unwrap();
    assert!(ranks[0].sides[0][0] < 0.0 && ranks[2].sides[0][0] > 0.0);
    assert!(ranks[1].sides[0][0].abs() < 1e-9);
}

#[test]
fn normalisation_names_parse() {
    assert_eq!(parse_normalisation("z-score"), Some(Normalisation::ZScore));
    assert_eq!(parse_normalisation("zscore"), Some(Normalisation::ZScore));
    assert_eq!(
        parse_normalisation("van_albada"),
        Some(Normalisation::VanAlbada)
    );
    assert_eq!(parse_normalisation("None"), Some(Normalisation::None));
    assert_eq!(parse_normalisation("x"), None);
}

#[test]
fn the_manifest_has_the_group_block_and_version_one_keys() {
    let fx = fixture("manifest", &["01", "02"]);
    let report = write_group_view(&fx.record, &fx.folder, &options(&fx)).unwrap();
    assert_eq!(
        report.manifest,
        fx.folder.join("viz").join("scene_manifest.json")
    );
    assert_eq!(report.patients, 2);
    assert_eq!(report.leads, 4);
    let m = read_manifest(&report.manifest);
    assert_eq!(m["manifest_version"], json!(2));
    assert_eq!(m["space"], json!(SPACE));
    assert!(m["template_nii"].as_str().unwrap().ends_with("t1.nii.gz"));
    let g = &m["group"];
    assert_eq!(g["guid"], json!(GUID));
    assert_eq!(g["colour_by"], json!("group"));
    assert_eq!(g["mirror"], json!(false));
    assert_eq!(g["show"]["active_contacts"], json!(true));
    assert_eq!(g["groups"].as_array().unwrap().len(), 2);
    let s = &g["subjects"][0];
    assert_eq!(s["subject"], json!("sub-01"));
    assert_eq!(s["block"], json!("mni"));
    assert_eq!(s["elmodel"], json!(MODEL));
    assert_eq!(
        s["active_contacts"]["1"],
        json!([false, true, false, false])
    );
    assert_eq!(
        s["active_contacts"]["2"],
        json!([false, false, false, false])
    );
    assert!(s["vta"]["1"].is_null());
    assert!(s["reco_json"]
        .as_str()
        .unwrap()
        .ends_with("sub-01_desc-reconstruction.json"));
    assert!(fx.deriv.join("sub-01").is_dir());
}

#[test]
fn a_clinical_variable_writes_values_and_the_colour_range() {
    let mut fx = fixture("clinical", &["01", "02"]);
    let mut v = ClinicalVar::blank("UPDRS", ClinicalKind::PerPatient, 2, 1);
    v.values = vec![row(&[10.0]), row(&[30.0])];
    fx.record.clinical.push(v);
    let mut o = options(&fx);
    o.colour_by = ColourBy::Clinical("UPDRS".to_owned());
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    assert_eq!(report.range, Some([10.0, 30.0]));
    let m = read_manifest(&report.manifest);
    let c = &m["group"]["colour_by"];
    assert_eq!(c["clinical"], json!("UPDRS"));
    assert_eq!(c["normalisation"], json!("none"));
    assert_eq!(c["colormap"], json!(COLORMAP));
    assert_eq!(c["range"], json!([10.0, 30.0]));
    assert_eq!(m["group"]["show"]["point_cloud"], json!(true));
    let s = &m["group"]["subjects"][1];
    assert_eq!(s["value"]["1"], json!([30.0, 30.0, 30.0, 30.0]));
    assert_eq!(s["value_kind"], json!("contact"));
}

#[test]
fn an_unknown_variable_and_a_bad_width_are_errors() {
    let mut fx = fixture("errors", &["01"]);
    let mut o = options(&fx);
    o.colour_by = ColourBy::Clinical("nope".to_owned());
    assert!(write_group_view(&fx.record, &fx.folder, &o).is_err());
    let mut v = ClinicalVar::blank("bad", ClinicalKind::PerContact, 1, 5);
    v.values = vec![row(&[1.0, 2.0, 3.0, 4.0, 5.0])];
    fx.record.clinical.push(v);
    o.colour_by = ColourBy::Clinical("bad".to_owned());
    let err = write_group_view(&fx.record, &fx.folder, &o).unwrap_err();
    assert!(err.to_string().contains("E_GROUP_ISOMATRIX"), "{err}");
}

#[test]
fn a_patient_without_a_reconstruction_is_skipped() {
    let mut fx = fixture("skip", &["01"]);
    let d = fx.record.datasets[0].id.clone();
    fx.record.add_patient("sub-99", &d, 1).unwrap();
    let report = write_group_view(&fx.record, &fx.folder, &options(&fx)).unwrap();
    assert_eq!(report.patients, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].0, "sub-99");
}

fn identity_flip(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let identity =
        TransformChain::from_queue(vec![TransformElem::Affine(AffineTransform::identity())]);
    let forward = dir.join(forward_name(SPACE));
    write_h5_file(&forward, &identity).unwrap();
    write_h5_file(&dir.join(inverse_name(SPACE)), &identity).unwrap();
    forward
}

#[test]
fn a_mirrored_copy_swaps_the_sides_and_flips_x() {
    let fx = fixture("mirror", &["01"]);
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_transform = FlipTransformChoice::Path(identity_flip(&fx.folder.join("flip")));
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    assert_eq!(report.leads, 4);
    let m = read_manifest(&report.manifest);
    assert_eq!(m["group"]["mirror"], json!(true));
    let subjects = m["group"]["subjects"].as_array().unwrap();
    assert_eq!(subjects.len(), 2);
    let copy = &subjects[1];
    assert_eq!(copy["subject"], json!("sub-01_mirrored"));
    assert_eq!(copy["mirrored"], json!(true));
    assert_eq!(copy["source_subject"], json!("sub-01"));
    assert_eq!(
        copy["active_contacts"]["2"],
        json!([false, true, false, false])
    );
    assert_eq!(
        copy["active_contacts"]["1"],
        json!([false, false, false, false])
    );
    let mirrored: Value = serde_json::from_str(
        &std::fs::read_to_string(copy["reco_json"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let head_r = mirrored["mni"]["1"]["markers"]["head"].as_array().unwrap();
    assert!(
        (head_r[0].as_f64().unwrap() - 10.0).abs() < 1e-3,
        "{head_r:?}"
    );
    let head_l = mirrored["mni"]["2"]["markers"]["head"].as_array().unwrap();
    assert!((head_l[0].as_f64().unwrap() + 10.0).abs() < 1e-3);
    let contacts = mirrored["mni"]["1"]["coords_mm"].as_array().unwrap();
    assert_eq!(contacts.len(), electrode_by_name(MODEL).unwrap().n_contacts);
    assert!(contacts.iter().all(|c| c[0].as_f64().unwrap() > 0.0));
}

#[test]
fn mirroring_without_a_cached_transform_warns_and_uses_the_plain_flip() {
    let fx = fixture("plain", &["01"]);
    let mut o = options(&fx);
    o.mirror = true;
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    assert!(
        report.notes.iter().any(|n| n.text.contains("plain x flip")),
        "{:?}",
        report.notes
    );
    let m = read_manifest(&report.manifest);
    assert_eq!(m["group"]["subjects"].as_array().unwrap().len(), 2);
}

#[test]
fn mirrored_values_swap_the_sides() {
    let v = PatientValues {
        kind: ValueKind::Contact,
        sides: [vec![1.0, 2.0], vec![3.0]],
    };
    let m = mirror_values(&v);
    assert_eq!(m.sides[0], vec![3.0]);
    assert_eq!(m.sides[1], vec![1.0, 2.0]);
}

#[test]
fn flipped_vta_files_are_listed_and_swap_for_the_copy() {
    let fx = fixture("vta", &["01"]);
    let dir = label_dir(&fx.deriv.join("sub-01"), SPACE, &format!("gs_{GUID}"));
    std::fs::create_dir_all(&dir).unwrap();
    let aff: fastwarp_core::Mat4 = [
        [1.0, 0.0, 0.0, 2.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let dims = [4, 2, 2];
    let write = |n: &str, a: &fastwarp_core::Mat4| {
        crate::vat::outputs::write_binary_nii(
            &dir.join(n),
            dims,
            &[1; 16],
            a,
            crate::vat::outputs::Codes::default(),
        )
        .unwrap();
    };
    let original = "sub-01_sim-binary_model-kuncel_hemi-R.nii";
    let flipped = "sub-01_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii";
    write(original, &aff);
    write(flipped, &aff);
    let mut o = options(&fx);
    o.mirror = true;
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    let m = read_manifest(&report.manifest);
    assert!(m["group"]["subjects"][0]["flipped"]["2"].is_null());
    assert!(report
        .notes
        .iter()
        .any(|n| n.to_string().contains("out-of-date") && n.to_string().contains("sub-01")));

    write(flipped, &super::super::flip::mirrored_affine(dims, &aff));
    let mut o = options(&fx);
    o.mirror = true;
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    let m = read_manifest(&report.manifest);
    let s = &m["group"]["subjects"][0];
    assert!(s["vta"]["1"].as_str().unwrap().ends_with("hemi-R.nii"));
    assert!(s["vta"]["2"].is_null());
    assert!(s["flipped"]["2"]
        .as_str()
        .unwrap()
        .ends_with("FlippedFromRight.nii"));
    let c = &m["group"]["subjects"][1];
    assert!(c["vta"]["2"]
        .as_str()
        .unwrap()
        .ends_with("FlippedFromRight.nii"));
    assert!(c["vta"]["1"].is_null());

    let left = "sub-01_sim-binary_model-kuncel_hemi-L.nii";
    let stale = "sub-01_sim-binary_model-kuncel_hemi-R_hemidesc-FlippedFromLeft.nii";
    write(left, &aff);
    write(stale, &aff);
    let mut o = options(&fx);
    o.mirror = true;
    let report = write_group_view(&fx.record, &fx.folder, &o).unwrap();
    let m = read_manifest(&report.manifest);
    let s = &m["group"]["subjects"][0];
    assert!(s["flipped"]["1"].is_null());
    assert!(s["flipped"]["2"]
        .as_str()
        .unwrap()
        .ends_with("FlippedFromRight.nii"));
    let notes: Vec<String> = report.notes.iter().map(ToString::to_string).collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains(stale) && n.contains("out of date")),
        "{notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains("no flipped VTA files")),
        "{notes:?}"
    );
}

#[test]
fn a_subject_in_another_space_needs_the_reference_block() {
    let fx = fixture("space", &["01"]);
    let path = fx
        .deriv
        .join("sub-01")
        .join("reconstruction")
        .join("sub-01_desc-reconstruction.json");
    let mut reco: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    reco["meta"]["template_space"] = json!("MNI152NLin6Asym");
    std::fs::write(&path, reco.to_string()).unwrap();
    let err = write_group_view(&fx.record, &fx.folder, &options(&fx)).unwrap_err();
    assert!(err.to_string().contains("no patient"), "{err}");
    reco["reference"] =
        json!({"space": SPACE, "1": side([11.0, -12.0, -8.0]), "2": side([-11.0, -12.0, -8.0])});
    std::fs::write(&path, reco.to_string()).unwrap();
    let report = write_group_view(&fx.record, &fx.folder, &options(&fx)).unwrap();
    let m = read_manifest(&report.manifest);
    assert_eq!(m["group"]["subjects"][0]["block"], json!("reference"));
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / n, v[1] / n, v[2] / n]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn lead_side(model: &str, head: [f64; 3], dir: [f64; 3]) -> Value {
    let g = electrode_by_name(model).unwrap();
    let dir = unit(dir);
    let tail = [
        head[0] + dir[0] * g.fiducial_span_mm(),
        head[1] + dir[1] * g.fiducial_span_mm(),
        head[2] + dir[2] * g.fiducial_span_mm(),
    ];
    let perp = unit(cross(dir, [0.0, 1.0, 0.0]));
    let x_raw = [head[0] + perp[0], head[1] + perp[1], head[2] + perp[2]];
    let side =
        dbsloc_reconstruct::native_frame::resolve_fiducials([head, tail, x_raw, head], g).unwrap();
    json!({
        "markers": {"head": side.head, "tail": side.tail, "x": side.x, "y": side.y},
        "coords_mm": side.coords_mm,
        "trajectory": side.trajectory,
    })
}

fn write_sphere(path: &Path, centre: [f64; 3], radius: f64) {
    let n = 40usize;
    let step = 0.5;
    let origin = [centre[0] - 10.0, centre[1] - 10.0, centre[2] - 10.0];
    let mut data = ndarray::Array3::<u8>::zeros((n, n, n));
    for i in 0..n {
        for j in 0..n {
            for k in 0..n {
                let p = [
                    origin[0] + step * i as f64,
                    origin[1] + step * j as f64,
                    origin[2] + step * k as f64,
                ];
                let d = ((p[0] - centre[0]).powi(2)
                    + (p[1] - centre[1]).powi(2)
                    + (p[2] - centre[2]).powi(2))
                .sqrt();
                if d <= radius {
                    data[[i, j, k]] = 1;
                }
            }
        }
    }
    let aff = [
        [step, 0.0, 0.0, origin[0]],
        [0.0, step, 0.0, origin[1]],
        [0.0, 0.0, step, origin[2]],
        [0.0, 0.0, 0.0, 1.0],
    ];
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::nifti_io::write_u8_3d(path, &data, &aff).unwrap();
}

#[test]
#[ignore = "writes a synthetic group of 6 patients into DBSLOC_GROUP_FIXTURE_DIR for screenshots"]
fn synthetic_group_fixture() {
    let Some(dir) = std::env::var_os("DBSLOC_GROUP_FIXTURE_DIR").map(PathBuf::from) else {
        return;
    };
    let root = dir.join("Cohort");
    let deriv = root.join("derivatives").join("leaddbs");
    let location = super::super::locate(&root, GUID, None, None).unwrap();
    let (mut record, json_path, _) =
        super::super::create_group(&location, "Synthetic cohort", None).unwrap();
    let dataset = record.ensure_dataset(&root.to_string_lossy(), &deriv.to_string_lossy());
    let models = [
        "Medtronic 3389",
        "Medtronic 3389",
        "Boston Scientific Vercise Directed",
        "Medtronic 3389",
        "Boston Scientific Vercise Directed",
        "Medtronic 3389",
    ];
    let label = format!("gs_{GUID}");
    for (n, model) in models.iter().enumerate() {
        let id = format!("{:02}", n + 1);
        let sd = deriv.join(format!("sub-{id}"));
        let jitter = |a: f64| a + 2.2 * ((n * 7 + 3) % 5) as f64 - 4.4;
        let right = lead_side(
            model,
            [jitter(11.0), jitter(-13.0), jitter(-8.0)],
            [0.20, 0.45, 0.87],
        );
        let left = lead_side(
            model,
            [-jitter(11.0), jitter(-13.5), jitter(-8.0)],
            [-0.20, 0.45, 0.87],
        );
        let reco = json!({
            "version": 2,
            "subj_id": id,
            "props": {"1": {"elmodel": model}, "2": {"elmodel": model}},
            "meta": {"side_keys": "1=right,2=left", "template_space": SPACE, "elmodel": model},
            "mni": {"1": right, "2": left},
        });
        let rec_dir = sd.join("reconstruction");
        std::fs::create_dir_all(&rec_dir).unwrap();
        std::fs::write(
            rec_dir.join(format!("sub-{id}_desc-reconstruction.json")),
            serde_json::to_string_pretty(&reco).unwrap(),
        )
        .unwrap();
        let contacts = electrode_by_name(model).unwrap().n_contacts;
        let mut stim = Stimulation::blank(&label, &id, ModelKind::Kuncel, [contacts, contacts]);
        for side in Side::ALL {
            let src = &mut stim.sides.get_mut(side).sources[0];
            src.mode = StimMode::Voltage;
            src.amplitude = 2.5;
            src.contacts[1] = Contact {
                index: 2,
                polarity: Polarity::Cathode,
                percent: 100.0,
                impedance_kohm: 1.0,
            };
        }
        crate::vat::settings::save(&sd, &stim).unwrap();
        let ldir = label_dir(&sd, SPACE, &label);
        for (side, letter, block) in [(Side::Right, 'R', &right), (Side::Left, 'L', &left)] {
            let c = block["coords_mm"][1].as_array().unwrap();
            let centre = [
                c[0].as_f64().unwrap(),
                c[1].as_f64().unwrap(),
                c[2].as_f64().unwrap(),
            ];
            let _ = side;
            write_sphere(
                &ldir.join(format!(
                    "sub-{id}_sim-binary_model-kuncel_hemi-{letter}.nii"
                )),
                centre,
                2.2 + 0.2 * (n % 3) as f64,
            );
        }
        record
            .add_patient(&format!("sub-{id}"), &dataset, 1 + (n % 2) as u32)
            .unwrap();
    }
    record.groups[0].label = "Responders".to_owned();
    record.groups[1].label = "Non-responders".to_owned();
    let mut var = ClinicalVar::blank("UPDRS improvement (%)", ClinicalKind::PerPatient, 6, 1);
    var.values = [62.0, 48.0, 15.0, 55.0, 30.0, 71.0]
        .iter()
        .map(|v| vec![Some(*v)])
        .collect();
    record.clinical.push(var);
    record.view.highlight_active = true;
    super::super::save_group(&json_path, &mut record).unwrap();
    println!("{}", json_path.display());
    std::fs::write(
        dir.join("group_path.txt"),
        json_path.to_string_lossy().as_bytes(),
    )
    .unwrap();
}
