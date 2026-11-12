#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use dbsloc_reconstruct::electrode_by_name;
use serde_json::json;

use super::*;
use crate::electrode_mesh::build_electrode;

const RING: &str = "Medtronic 3389";
const DIRECTED: &str = "Boston Scientific Vercise Directed";

fn side_json(model: &str, head: [f64; 3]) -> serde_json::Value {
    let g = electrode_by_name(model).unwrap();
    let r = 0.5 * g.lead_diameter_mm;
    let tail = [head[0], head[1], head[2] + g.contact_span_mm()];
    let contacts: Vec<[f64; 3]> = g
        .contact_xyz_mm
        .iter()
        .map(|p| [head[0] + p[0], head[1] + p[1], head[2] + p[2]])
        .collect();
    json!({
        "markers": {
            "head": head, "tail": tail,
            "x": [head[0] + r, head[1], head[2]],
            "y": [head[0], head[1] + r, head[2]],
        },
        "coords_mm": contacts,
    })
}

fn write_reco(dir: &Path, name: &str, model: &str, dx: f64) -> PathBuf {
    let p = dir.join(format!("{name}.json"));
    let reco = json!({
        "version": 2,
        "meta": {"side_keys": "1=right,2=left", "elmodel": model},
        "mni": {
            "1": side_json(model, [10.0 + dx, -12.0, -8.0]),
            "2": side_json(model, [-10.0 - dx, -12.0, -8.0]),
        },
    });
    std::fs::write(&p, reco.to_string()).unwrap();
    p
}

fn subject(name: &str, reco: &Path, model: &str, group: u32, value: f64) -> serde_json::Value {
    let k = electrode_by_name(model).unwrap().n_contacts;
    json!({
        "subject": name, "label": format!("Group {group}"), "group": group,
        "reco_json": reco, "block": "mni", "elmodel": model,
        "active_contacts": {"1": (0..k).map(|i| i == 1).collect::<Vec<_>>(), "2": vec![false; k]},
        "vta": {"1": null, "2": null},
        "value": {"1": vec![value; k], "2": vec![serde_json::Value::Null; k]},
        "value_kind": "contact",
    })
}

fn manifest_value(dir: &Path, colour: serde_json::Value) -> serde_json::Value {
    let a = write_reco(dir, "a", RING, 0.0);
    let b = write_reco(dir, "b", RING, 2.0);
    let c = write_reco(dir, "c", DIRECTED, 4.0);
    let template = dir.join("t1.nii.gz");
    std::fs::write(&template, b"x").unwrap();
    json!({
        "manifest_version": 2,
        "subj_id": "group-1",
        "space": "MNI152NLin2009bAsym",
        "template_nii": template,
        "group": {
            "guid": "1",
            "subjects": [
                subject("sub-a", &a, RING, 1, 1.0),
                subject("sub-b", &b, RING, 2, 3.0),
                subject("sub-c", &c, DIRECTED, 2, 5.0),
            ],
            "groups": [
                {"number": 1, "label": "STN", "color": [0.2, 0.4, 0.8]},
                {"number": 2, "label": "GPi", "color": [0.8, 0.4, 0.2]},
            ],
            "colour_by": colour,
            "mirror": false,
            "show": {"active_contacts": true, "passive_contacts": true,
                     "highlight_active": false, "vtas": true, "point_cloud": false,
                     "isovolume": false},
        },
    })
}

fn load(colour: serde_json::Value) -> (tempfile::TempDir, GroupScene) {
    let dir = tempfile::tempdir().unwrap();
    let v = manifest_value(dir.path(), colour);
    let manifest = dir.path().join("scene_manifest.json");
    std::fs::write(&manifest, v.to_string()).unwrap();
    let m = SceneManifestV1::from_json_file(&manifest).unwrap();
    let scene = GroupScene::from_manifest(&m).unwrap();
    (dir, scene)
}

fn display(scene: &GroupScene) -> GroupDisplay {
    GroupDisplay::from_show(&scene.show)
}

#[test]
fn version_two_parses_with_a_group_block_and_version_one_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let v = manifest_value(dir.path(), json!("group"));
    let manifest = dir.path().join("m2.json");
    std::fs::write(&manifest, v.to_string()).unwrap();
    let m = SceneManifestV1::from_json_file(&manifest).unwrap();
    assert_eq!(m.manifest_version, 2);
    assert!(m.reco_json.as_os_str().is_empty());
    let g = m.group.as_ref().unwrap();
    assert_eq!(g.subjects.len(), 3);
    assert_eq!(g.groups[1].label, "GPi");
    assert!(matches!(g.colour_by, GroupColourByV2::Name(ref n) if n == "group"));

    let reco = write_reco(dir.path(), "r", RING, 0.0);
    let template = dir.path().join("t1.nii.gz");
    let v1 = json!({"manifest_version": 1, "subj_id": "01", "template_nii": template, "reco_json": reco});
    let p1 = dir.path().join("m1.json");
    std::fs::write(&p1, v1.to_string()).unwrap();
    let m1 = SceneManifestV1::from_json_file(&p1).unwrap();
    assert_eq!(m1.manifest_version, 1);
    assert!(m1.group.is_none());
    assert!(m1.reco_json.is_file());

    let mut bad = v.clone();
    bad.as_object_mut().unwrap().remove("group");
    let p3 = dir.path().join("m3.json");
    std::fs::write(&p3, bad.to_string()).unwrap();
    assert!(SceneManifestV1::from_json_file(&p3).is_err());
    let mut bad = v;
    bad["manifest_version"] = json!(3);
    std::fs::write(&p3, bad.to_string()).unwrap();
    assert!(SceneManifestV1::from_json_file(&p3).is_err());
}

#[test]
fn a_clinical_colouring_has_its_range_and_missing_values_are_not_coloured() {
    let (_d, scene) = load(json!({
        "clinical": "UPDRS", "normalisation": "none", "colormap": "blue_white_red",
        "range": [1.0, 5.0],
    }));
    assert_eq!(
        scene.colour,
        ColourMode::Clinical {
            name: "UPDRS".into(),
            normalisation: "none".into(),
            range: [1.0, 5.0]
        }
    );
    match scene.legend() {
        Legend::ColourBar { name, range, .. } => {
            assert_eq!(name, "UPDRS");
            assert_eq!(range, [1.0, 5.0]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(value_colour(1.0, [1.0, 5.0]), Some([0.0, 0.0, 1.0]));
    assert_eq!(value_colour(3.0, [1.0, 5.0]), Some([1.0, 1.0, 1.0]));
    assert_eq!(value_colour(5.0, [1.0, 5.0]), Some([1.0, 0.0, 0.0]));
    assert_eq!(value_colour(f32::NAN, [1.0, 5.0]), None);
    assert_eq!(unit_position(2.0, [2.0, 2.0]), 0.5);
    let left = scene
        .leads
        .iter()
        .find(|l| l.subject == "sub-a" && l.side == 1)
        .unwrap();
    assert!(left.values.as_ref().unwrap().iter().all(|v| v.is_nan()));
}

#[test]
fn the_range_falls_back_to_the_values_and_the_legend_lists_the_groups() {
    let (_d, scene) = load(json!({"clinical": "x"}));
    match &scene.colour {
        ColourMode::Clinical { range, .. } => assert_eq!(*range, [1.0, 5.0]),
        other => panic!("{other:?}"),
    }
    let (_d, scene) = load(json!("group"));
    match scene.legend() {
        Legend::Groups(g) => {
            assert_eq!(g.len(), 2);
            assert_eq!(g[0].0, "STN");
            assert_eq!(g[1].1, [0.8, 0.4, 0.2]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn instances_are_counted_per_model_and_part() {
    let (_d, scene) = load(json!("group"));
    assert_eq!(scene.leads.len(), 6);
    let per_model = scene.leads_per_model();
    assert_eq!(per_model[RING], 4);
    assert_eq!(per_model[DIRECTED], 2);
    let names = scene.model_names();
    assert_eq!(names.len(), 2);
    let models = GroupModels::build(&names);
    assert_eq!(models.mesh_count(), 2);
    assert!(models.sphere.is_some());
    let batches = scene.build_batches(&display(&scene), &models);
    let count = |model: &str, part: Part| {
        batches
            .iter()
            .find(|b| b.model.as_deref() == Some(model) && b.part == Some(part))
            .map_or(0, |b| b.opaque.len() + b.dimmed.len())
    };
    assert_eq!(count(RING, Part::Insulation), 4);
    assert_eq!(count(RING, Part::Metal), 4);
    assert_eq!(count(DIRECTED, Part::Insulation), 2);
    assert_eq!(count(DIRECTED, Part::Metal), 2);
    assert!(batches.iter().all(|b| b.part != Some(Part::Sphere)));
}

#[test]
fn highlight_active_draws_red_full_size_markers_and_half_size_passive_ones() {
    let (_d, scene) = load(json!("group"));
    let models = GroupModels::build(&scene.model_names());
    let mut d = display(&scene);
    d.highlight_active = true;
    let batches = scene.build_batches(&d, &models);
    let spheres = batches
        .iter()
        .find(|b| b.part == Some(Part::Sphere))
        .unwrap();
    let red: Vec<&Instance> = spheres
        .opaque
        .iter()
        .filter(|i| i.colour[..3] == ACTIVE_RGB)
        .collect();
    assert_eq!(red.len(), 3);
    assert!(red
        .iter()
        .all(|i| i.m0[0] == MARKER_RADIUS_MM && i.extra[0] == 1.0));
    let small = spheres
        .opaque
        .iter()
        .filter(|i| i.m0[0] == 0.5 * MARKER_RADIUS_MM)
        .count();
    assert_eq!(small, spheres.opaque.len() - 3);
    d.show_passive = false;
    let only = scene.build_batches(&d, &models);
    let spheres = only.iter().find(|b| b.part == Some(Part::Sphere)).unwrap();
    assert_eq!(spheres.opaque.len(), 3);
    d.show_active = false;
    d.show_passive = true;
    d.highlight_active = false;
    let passive = scene.build_batches(&d, &models);
    let spheres = passive
        .iter()
        .find(|b| b.part == Some(Part::Sphere))
        .unwrap();
    assert!(spheres.opaque.iter().all(|i| i.colour[..3] != ACTIVE_RGB));
}

#[test]
fn the_point_cloud_skips_missing_values() {
    let (_d, scene) = load(json!({"clinical": "x", "range": [1.0, 5.0]}));
    let models = GroupModels::build(&scene.model_names());
    let mut d = display(&scene);
    d.point_cloud = true;
    let batches = scene.build_batches(&d, &models);
    let spheres = batches
        .iter()
        .find(|b| b.part == Some(Part::Sphere))
        .unwrap();
    let expected: usize = scene
        .leads
        .iter()
        .filter(|l| l.side == 0)
        .map(|l| l.contacts.len())
        .sum();
    assert_eq!(spheres.opaque.len(), expected);
    assert!(spheres.opaque.iter().all(|i| i.m0[0] == POINT_RADIUS_MM));
    let blue = spheres
        .opaque
        .iter()
        .filter(|i| i.colour[..3] == [0.0, 0.0, 1.0])
        .count();
    let first = scene
        .leads
        .iter()
        .find(|l| l.subject == "sub-a" && l.side == 0)
        .unwrap();
    assert_eq!(blue, first.contacts.len());
    d.show_passive = false;
    let active = scene.build_batches(&d, &models);
    let spheres = active
        .iter()
        .find(|b| b.part == Some(Part::Sphere))
        .unwrap();
    assert_eq!(spheres.opaque.len(), 3);
}

#[test]
fn a_selection_dims_the_other_leads() {
    let (_d, scene) = load(json!("group"));
    let models = GroupModels::build(&scene.model_names());
    let mut d = display(&scene);
    d.selected_patient = Some("sub-a".into());
    let batches = scene.build_batches(&d, &models);
    let ring_ins = batches
        .iter()
        .find(|b| b.model.as_deref() == Some(RING) && b.part == Some(Part::Insulation))
        .unwrap();
    assert_eq!(ring_ins.opaque.len(), 2);
    assert_eq!(ring_ins.dimmed.len(), 2);
    assert!(ring_ins.dimmed.iter().all(|i| i.colour[3] == DIMMED_ALPHA));
    d.selected_patient = None;
    d.selected_group = Some(2);
    let batches = scene.build_batches(&d, &models);
    let dir_ins = batches
        .iter()
        .find(|b| b.model.as_deref() == Some(DIRECTED) && b.part == Some(Part::Insulation))
        .unwrap();
    assert_eq!(dir_ins.opaque.len(), 2);
    assert!(dir_ins.dimmed.is_empty());
}

#[test]
fn the_instance_matrix_places_the_canonical_lead_like_a_direct_build() {
    let g = electrode_by_name(RING).unwrap();
    let head = Vec3::new(10.0, -12.0, -8.0);
    let tail = head + Vec3::new(0.5, 0.2, 9.0);
    let x = head + Vec3::new(0.6, 0.0, -0.1);
    let rows = lead_instance_matrix(g, head, tail, Some(x), None).unwrap();
    let canonical = canonical_electrode(g).unwrap();
    let direct = build_electrode(g, head, tail, Some(x), None, 30.0).unwrap();
    let place = |p: Vec3| {
        let q = [p.x, p.y, p.z, 1.0];
        let row = |r: [f32; 4]| r.iter().zip(q).map(|(a, b)| a * b).sum::<f32>();
        Vec3::new(row(rows[0]), row(rows[1]), row(rows[2]))
    };
    let mapped: Vec<Vec3> = canonical
        .metal
        .positions
        .iter()
        .map(|p| place(*p))
        .collect();
    assert_eq!(mapped.len(), direct.metal.positions.len());
    for (a, b) in mapped.iter().zip(&direct.metal.positions) {
        assert!((*a - *b).length() < 1e-3, "{a:?} {b:?}");
    }
}

#[test]
fn picking_returns_the_nearest_lead_and_the_hover_text_names_it() {
    let (_d, scene) = load(json!({"clinical": "UPDRS", "range": [1.0, 5.0]}));
    let project = |p: Vec3| Some([p.x * 10.0, p.z * 10.0]);
    let lead = scene
        .leads
        .iter()
        .position(|l| l.subject == "sub-b" && l.side == 0)
        .unwrap();
    let c = scene.leads[lead].contacts[1];
    let hit = scene
        .pick(&project, [c.x * 10.0 + 1.0, c.z * 10.0 + 1.0], 8.0)
        .unwrap();
    assert_eq!(hit.lead, lead);
    assert_eq!(hit.point, Some(1));
    assert_eq!(scene.hover_text(&hit), "sub-b · Group 2 · right · 3.00");
    assert!(scene.pick(&project, [5000.0, 5000.0], 8.0).is_none());
}

#[test]
fn the_default_camera_target_is_the_mean_of_all_contacts() {
    let (_d, scene) = load(json!("group"));
    let (mut sum, mut n) = (Vec3::ZERO, 0.0);
    for l in &scene.leads {
        for c in &l.contacts {
            sum += *c;
            n += 1.0;
        }
    }
    assert!((scene.centre - sum / n).length() < 1e-4);
    assert_eq!(scene.patients(), vec!["sub-a", "sub-b", "sub-c"]);
}

#[test]
fn a_vta_mesh_cache_round_trips() {
    let mesh = unit_sphere(4, 6);
    let bytes = encode_mesh(&mesh);
    let back = decode_mesh(&bytes).unwrap();
    assert_eq!(back.positions, mesh.positions);
    assert_eq!(back.indices, mesh.indices);
    assert!(decode_mesh(&bytes[..bytes.len() - 1]).is_none());
    assert!(decode_mesh(b"nonsense").is_none());
}

#[test]
fn contact_pairs_are_drawn_at_their_midpoints() {
    let (_d, mut scene) = load(json!("group"));
    let lead = &mut scene.leads[0];
    lead.pair_values = true;
    lead.values = Some(vec![1.0, 2.0, 3.0]);
    let pts = GroupScene::contact_points(lead);
    assert_eq!(pts.len(), lead.contacts.len() - 1);
    assert!((pts[0].pos - 0.5 * (lead.contacts[0] + lead.contacts[1])).length() < 1e-6);
    assert_eq!(pts[2].value, 3.0);
    assert!(pts[0].active, "pair 0-1 contains the active contact 1");
}

#[test]
fn a_group_payload_loads_with_the_mean_contact_as_camera_target() {
    let dir = tempfile::tempdir().unwrap();
    let n = 8usize;
    let voxels: Vec<f32> = (0..n * n * n).map(|i| (i % 7) as f32).collect();
    let template = dir.path().join("t1.nii");
    std::fs::write(
        &template,
        dbsloc_checkreg::nifti_io::make_nifti1_bytes(n, n, n, &voxels),
    )
    .unwrap();
    let mut v = manifest_value(dir.path(), json!("group"));
    v["template_nii"] = json!(template);
    let manifest = dir.path().join("scene_manifest.json");
    std::fs::write(&manifest, v.to_string()).unwrap();
    let m = SceneManifestV1::from_json_file(&manifest).unwrap();
    let payload = crate::Scene3DPayload::from_manifest_fast(&m).unwrap();
    let group = payload.group().expect("group scene");
    assert_eq!(group.leads.len(), 6);
    assert_eq!(payload.view_defaults.target, group.centre);
    let display = payload.group_display().unwrap();
    assert!(display.show_active && display.show_passive && !display.highlight_active);
    let mut changed = display.clone();
    changed.selected_group = Some(1);
    payload.set_group_display(changed.clone());
    assert_eq!(payload.group_display().unwrap(), changed);
    assert!(payload.points_l.is_empty() && payload.electrode_meshes.is_empty());
}
