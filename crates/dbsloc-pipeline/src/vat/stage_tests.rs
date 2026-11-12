#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use dbsloc_reconstruct::electrode_by_name;
use dbsloc_vat::geometry::contact_centre_world;
use dbsloc_vat::LeadFrame;
use fastwarp_matfile::mat5::read_mat5_file;
use fastwarp_reg::transform::itk_h5::write_h5_file;
use fastwarp_reg::transform::{AffineTransform, TransformChain, TransformElem};
use serde_json::{json, Value};

use super::*;
use crate::bids::resolve_subject_with;
use crate::options::VatBackend;
use crate::vat::settings::{CaseSpec, Contact, Mode, Polarity};

const SPACE: &str = "MNI152NLin2009bAsym";
const MODEL: &str = "Medtronic 3389";
const SHIFT: [f64; 3] = [3.0, -2.0, 4.0];
const HEAD_R: [f64; 3] = [10.0, -12.0, -8.0];
const HEAD_L: [f64; 3] = [-10.0, -12.0, -8.0];

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    sub: Subject,
    atlases: PathBuf,
}

fn small_params() -> VatParams {
    VatParams {
        grid: GridSpec {
            fine_spacing_mm: 0.5,
            fine_half_width_mm: 3.0,
            fine_margin_z_mm: 3.0,
            growth: 1.5,
            domain_margin_mm: 12.0,
            max_cells: 5_000_000,
            output_spacing_mm: 0.5,
        },
        gm_spacing_mm: 1.0,
        device: crate::reg_backend::shared_backend,
    }
}

fn markers(head: [f64; 3]) -> Value {
    let model = electrode_by_name(MODEL).unwrap();
    let r = 0.5 * model.lead_diameter_mm;
    let tail = [head[0], head[1], head[2] + model.contact_span_mm()];
    json!({
        "head": head,
        "tail": tail,
        "x": [head[0] + r, head[1], head[2]],
        "y": [head[0], head[1] + r, head[2]],
    })
}

fn write_atlas(dir: &Path, centre_template: [f64; 3]) -> [i64; 3] {
    let lo = [
        centre_template[0].round() as i64,
        centre_template[1].round() as i64 - 15,
        centre_template[2].round() as i64 - 15,
    ];
    let set = dir.join("Synth").join("rh");
    std::fs::create_dir_all(&set).unwrap();
    let mut data = ndarray::Array3::<u8>::zeros((30, 30, 30));
    data.fill(1);
    let aff = [
        [1.0, 0.0, 0.0, lo[0] as f64],
        [0.0, 1.0, 0.0, lo[1] as f64],
        [0.0, 0.0, 1.0, lo[2] as f64],
        [0.0, 0.0, 0.0, 1.0],
    ];
    crate::nifti_io::write_u8_3d(&set.join("cube.nii"), &data, &aff).unwrap();
    lo
}

fn fixture(name: &str, with_native: bool, with_warp: bool) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(&format!("dbsloc_vat_{name}_"))
        .tempdir()
        .unwrap();
    let root = dir.path().to_path_buf();
    let anat = root.join("rawdata/sub-1/ses-preop/anat");
    std::fs::create_dir_all(&anat).unwrap();
    std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
    let probe = resolve_subject_with(&root, "1", false).unwrap();
    std::fs::create_dir_all(&probe.recon_dir).unwrap();
    let mut reco = json!({
        "version": 2,
        "subj_id": "1",
        "props": {"1": {"elmodel": MODEL}, "2": {"elmodel": MODEL}},
        "meta": {"side_keys": "1=right,2=left"},
    });
    if with_native {
        reco["native"] = json!({
            "1": {"markers": markers(HEAD_R)},
            "2": {"markers": markers(HEAD_L)},
        });
    }
    std::fs::write(
        probe.reconstruction_json(),
        serde_json::to_string_pretty(&reco).unwrap(),
    )
    .unwrap();
    if with_warp {
        std::fs::create_dir_all(crate::derivatives::norm_log_dir(&probe)).unwrap();
        std::fs::write(
            crate::derivatives::norm_method_log(&probe),
            format!(r#"{{"template_space": "{SPACE}"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(crate::derivatives::norm_transform_dir(&probe)).unwrap();
        let lps = [SHIFT[0], SHIFT[1], -SHIFT[2]];
        let forward = AffineTransform::from_translation(lps);
        let inverse = forward.inverse().unwrap();
        write_h5_file(
            &crate::derivatives::norm_forward(&probe, SPACE),
            &TransformChain::from_queue(vec![TransformElem::Affine(forward)]),
        )
        .unwrap();
        write_h5_file(
            &crate::derivatives::norm_inverse(&probe, SPACE),
            &TransformChain::from_queue(vec![TransformElem::Affine(inverse)]),
        )
        .unwrap();
    }
    let sub = resolve_subject_with(&root, "1", false).unwrap();
    let atlases = root.join("atlases");
    let model = electrode_by_name(MODEL).unwrap();
    let m = markers(HEAD_R);
    let as3 = |v: &Value| -> [f64; 3] {
        [
            v[0].as_f64().unwrap(),
            v[1].as_f64().unwrap(),
            v[2].as_f64().unwrap(),
        ]
    };
    let frame = LeadFrame::from_markers(
        as3(&m["head"]),
        as3(&m["tail"]),
        Some(as3(&m["x"])),
        Some(as3(&m["y"])),
        model,
    )
    .unwrap();
    let c = contact_centre_world(&frame, model, 1).unwrap();
    write_atlas(
        &atlases,
        [c[0] + SHIFT[0], c[1] + SHIFT[1], c[2] + SHIFT[2]],
    );
    Fixture {
        _dir: dir,
        root,
        sub,
        atlases,
    }
}

fn options(fx: &Fixture) -> RunOptions {
    let mut o = RunOptions::new(&fx.root, "1");
    o.el_model = None;
    o.leaddbs_atlases_dir = Some(fx.atlases.clone());
    o.vat_stats_atlas = Some("external:Synth".to_owned());
    o
}

fn stimulation(
    label: &str,
    model: ModelKind,
    side: Side,
    mode: Mode,
    amplitude: f64,
    contact: usize,
) -> Stimulation {
    let mut s = Stimulation::blank(label, "1", model, [4, 4]);
    s.model_settings.conductivity.mode = cfg::ConductivityMode::Homogeneous;
    s.model_settings.conductivity.homogeneous_s_per_m = Some(0.1);
    let src = &mut s.sides.get_mut(side).sources[0];
    src.mode = mode;
    src.amplitude = amplitude;
    src.contacts[contact - 1] = Contact {
        index: contact as u32,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 1.0,
    };
    src.case = CaseSpec {
        polarity: Polarity::Anode,
        percent: 100.0,
    };
    s
}

fn save(fx: &Fixture, s: &Stimulation) {
    cfg::save(&fx.sub.subj_dir, s).unwrap();
}

struct Ran {
    result: PipelineResult<StageOutcome>,
    lines: Vec<String>,
}

fn run(fx: &Fixture, opts: &RunOptions) -> Ran {
    run_with(fx, opts, &small_params())
}

fn no_device() -> Result<std::sync::Arc<fastwarp_gpu::WgpuBackend>, String> {
    Err("no adapter in this test".to_owned())
}

fn run_with(fx: &Fixture, opts: &RunOptions, params: &VatParams) -> Ran {
    let (tx, rx) = mpsc::channel();
    let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
    let result = run_vat_with(&sub, opts, params, None, Some(&tx));
    drop(tx);
    let lines = rx
        .into_iter()
        .filter_map(|e| match e {
            RunEvent::LogLine { line } => Some(line),
            _ => None,
        })
        .collect();
    Ran { result, lines }
}

fn token(r: &Ran) -> String {
    match &r.result {
        Ok(StageOutcome::Completed(t)) => t.clone(),
        Ok(StageOutcome::Skipped { result, .. }) => result.clone(),
        Err(e) => format!("error: {e}"),
    }
}

fn label_dir_of(fx: &Fixture, space: &str, label: &str) -> PathBuf {
    label_dir(&fx.sub.subj_dir, space, label)
}

fn json_of(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn centroid_and_count(path: &Path) -> ([f64; 3], usize) {
    let (vol, aff) = crate::nifti_io::load_volume_f64(path).unwrap();
    let (nx, ny, nz) = vol.dim();
    let mut sum = [0.0; 3];
    let mut n = 0usize;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                if vol[[i, j, k]] > 0.5 {
                    let w = fastwarp_core::apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                    for a in 0..3 {
                        sum[a] += w[a];
                    }
                    n += 1;
                }
            }
        }
    }
    let c = n as f64;
    ([sum[0] / c, sum[1] / c, sum[2] / c], n)
}

#[test]
fn a_kuncel_label_writes_every_file_for_the_stimulated_side_only() {
    let fx = fixture("kuncel", true, true);
    save(
        &fx,
        &stimulation("k3v", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    let ran = run(&fx, &options(&fx));
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    let names = Names::new("1", "kuncel");
    for space in ["native", SPACE] {
        let dir = label_dir_of(&fx, space, "k3v");
        for f in [
            names.binary_nii(Side::Right),
            names.hemi_mat(Side::Right),
            names.stimparameters_mat(),
            names.vta_json(),
        ] {
            assert!(dir.join(&f).is_file(), "{space}/{f}");
        }
        for f in [
            names.binary_nii(Side::Left),
            names.hemi_mat(Side::Left),
            names.efield_nii(Side::Right),
            names.efieldgauss_nii(Side::Right),
        ] {
            assert!(!dir.join(&f).exists(), "{space}/{f}");
        }
    }
    let tdir = label_dir_of(&fx, SPACE, "k3v");
    assert!(tdir.join(names.vtastats_json()).is_file());
    assert!(!label_dir_of(&fx, "native", "k3v")
        .join(names.vtastats_json())
        .exists());
    let side = &json_of(&label_dir_of(&fx, "native", "k3v").join(names.vta_json()))["sides"];
    assert_eq!(side["left"]["status"], "no_stimulation");
    assert_eq!(side["right"]["status"], "computed");
    let sphere = 4.0 / 3.0 * std::f64::consts::PI * 3.6307f64.powi(3);
    let final_mm3 = side["right"]["volume_final_mm3"].as_f64().unwrap();
    assert!((final_mm3 - sphere).abs() / sphere < 0.03, "{final_mm3}");
    let mat = read_mat5_file(&label_dir_of(&fx, "native", "k3v").join(names.hemi_mat(Side::Right)))
        .unwrap();
    let vol = mat.iter().find(|(n, _)| n == "vatvolume").unwrap();
    assert!(
        matches!(&vol.1, fastwarp_matfile::mat5::Mat5Value::Double { data, .. } if (data[0] - sphere).abs() < 0.01)
    );
    assert!(mat.iter().any(|(n, _)| n == "vatfv"));
    assert!(!mat.iter().any(|(n, _)| n == "vatgrad"));
    let lock = json_of(&stage_approval_path(&fx.sub.locks_dir(), "vat"));
    assert_eq!(lock["approval"], json!(false));
    let inputs: Vec<String> = lock["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap().replace('\\', "/"))
        .collect();
    for needle in [
        "_desc-reconstruction.json",
        "_desc-stimulation.json",
        "_from-anchorNative_to-MNI152NLin2009bAsym_mode-image_xfm.h5",
        "_from-MNI152NLin2009bAsym_to-anchorNative_mode-image_xfm.h5",
    ] {
        assert!(
            inputs.iter().any(|p| p.ends_with(needle)),
            "{needle}: {inputs:?}"
        );
    }
    assert!(fx.sub.subj_dir.join("sub-1_desc-stats.mat").is_file());
}

#[test]
fn the_template_vta_is_the_native_vta_moved_by_the_normalization() {
    let fx = fixture("move", true, true);
    save(
        &fx,
        &stimulation("mv", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    let ran = run(&fx, &options(&fx));
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    let names = Names::new("1", "kuncel");
    let (cn, nn) =
        centroid_and_count(&label_dir_of(&fx, "native", "mv").join(names.binary_nii(Side::Right)));
    let (ct, nt) =
        centroid_and_count(&label_dir_of(&fx, SPACE, "mv").join(names.binary_nii(Side::Right)));
    for a in 0..3 {
        assert!(
            (ct[a] - cn[a] - SHIFT[a]).abs() < 0.15,
            "{a}: {ct:?} {cn:?}"
        );
    }
    assert!(nn > 1000 && nt > 100, "{nn} {nt}");
}

#[test]
fn the_overlap_statistics_match_the_hand_computed_fractions() {
    let fx = fixture("stats", true, true);
    save(
        &fx,
        &stimulation("st", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    let ran = run(&fx, &options(&fx));
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    let names = Names::new("1", "kuncel");
    let tdir = label_dir_of(&fx, SPACE, "st");
    let (vol, aff) =
        crate::nifti_io::load_volume_f64(&tdir.join(names.binary_nii(Side::Right))).unwrap();
    let c = {
        let model = electrode_by_name(MODEL).unwrap();
        let m = markers(HEAD_R);
        let g = |k: &str| -> [f64; 3] {
            [
                m[k][0].as_f64().unwrap(),
                m[k][1].as_f64().unwrap(),
                m[k][2].as_f64().unwrap(),
            ]
        };
        let f = LeadFrame::from_markers(g("head"), g("tail"), Some(g("x")), Some(g("y")), model)
            .unwrap();
        let c = contact_centre_world(&f, model, 1).unwrap();
        [c[0] + SHIFT[0], c[1] + SHIFT[1], c[2] + SHIFT[2]]
    };
    let lo = [
        c[0].round() as i64,
        c[1].round() as i64 - 15,
        c[2].round() as i64 - 15,
    ];
    let inside =
        |w: [f64; 3]| (0..3).all(|a| w[a] > lo[a] as f64 - 0.5 && w[a] < lo[a] as f64 + 29.5);
    let (nx, ny, nz) = vol.dim();
    let (mut total, mut within) = (0usize, 0usize);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                if vol[[i, j, k]] > 0.5 {
                    total += 1;
                    let w = fastwarp_core::apply_rowmajor(&aff, [i as f64, j as f64, k as f64]);
                    if inside(w) {
                        within += 1;
                    }
                }
            }
        }
    }
    assert!(
        total > 0 && within > 0 && within < total,
        "{within}/{total}"
    );
    let rec = json_of(&tdir.join(names.vtastats_json()));
    let right = &rec["sides"]["right"];
    let vox = 0.5f64.powi(3);
    let want_n = within as f64 / total as f64;
    let got_n = right["n_intersection"][0].as_f64().unwrap();
    assert!((got_n - want_n).abs() < 1e-12, "{got_n} {want_n}");
    let got_mm3 = right["intersection_mm3"][0].as_f64().unwrap();
    assert!((got_mm3 - within as f64 * vox).abs() < 1e-9, "{got_mm3}");
    let atlas_volume = 30.0 * 30.0 * 30.0;
    assert!((right["atlas_volume_mm3"][0].as_f64().unwrap() - atlas_volume).abs() < 1e-9);
    let want_within = within as f64 * vox / atlas_volume;
    assert!((right["n_within_intersection"][0].as_f64().unwrap() - want_within).abs() < 1e-12);
    assert!((right["volume_mm3"].as_f64().unwrap() - total as f64 * vox).abs() < 1e-9);
    let left = &rec["sides"]["left"];
    assert_eq!(left["n_intersection"][0].as_f64().unwrap(), 0.0);
    assert!(left["atlas_volume_mm3"][0].is_null());
    assert_eq!(rec["atlas"]["structures"][0]["name"], "cube.nii");
    let stats = read_mat5_file(&fx.sub.subj_dir.join("sub-1_desc-stats.mat")).unwrap();
    let ea = &stats.iter().find(|(n, _)| n == "ea_stats").unwrap().1;
    let stim = ea.field("stimulation", 0).unwrap();
    assert!(
        matches!(stim.field("label", 0), Some(fastwarp_matfile::mat5::Mat5Value::Char(l)) if l == "st")
    );
    let vat = stim.field("vat", 0).unwrap();
    let inter = vat.field("nAtlasIntersection", 0).unwrap();
    assert!(
        matches!(inter, fastwarp_matfile::mat5::Mat5Value::Double { data, .. } if (data[0] - want_n).abs() < 1e-12)
    );
}

#[test]
fn an_efield_label_runs_is_kept_recomputed_and_reuses_its_basis() {
    let fx = fixture("efield", true, true);
    let mut s = stimulation("ef", ModelKind::Efield, Side::Right, Mode::Current, 2.0, 2);
    save(&fx, &s);
    let mut gs = stimulation(
        "gs_group",
        ModelKind::Kuncel,
        Side::Right,
        Mode::Voltage,
        3.0,
        2,
    );
    gs.label = "gs_group".to_owned();
    cfg::save(&fx.sub.subj_dir, &gs).unwrap();
    let mut opts = options(&fx);
    opts.vat_backend = VatBackend::Cpu;
    let first = run(&fx, &opts);
    assert_eq!(token(&first), "ok:vat:1/1", "{:?}", first.lines);
    let names = Names::new("1", "simbio");
    let ndir = label_dir_of(&fx, "native", "ef");
    for side_file in [
        names.binary_nii(Side::Right),
        names.efield_nii(Side::Right),
        names.efieldgauss_nii(Side::Right),
        names.hemi_mat(Side::Right),
    ] {
        assert!(ndir.join(&side_file).is_file(), "{side_file}");
    }
    assert!(!ndir.join(names.efield_nii(Side::Left)).exists());
    assert!(!label_dir_of(&fx, "native", "gs_group")
        .join("sub-1_desc-vta.json")
        .exists());
    let mat = read_mat5_file(&ndir.join(names.hemi_mat(Side::Right))).unwrap();
    assert!(mat.iter().any(|(n, _)| n == "vatgrad"));
    let sidecar = json_of(&ndir.join(names.vta_json()));
    assert_eq!(sidecar["engine"]["backend"], "cpu");
    assert_eq!(sidecar["engine"]["basis_cache"], "built");
    assert_eq!(sidecar["lead_dbs"]["slug"], "simbio");
    let (_, n) = centroid_and_count(&ndir.join(names.binary_nii(Side::Right)));
    assert!(n > 0);
    let gauss = {
        let (v, _) =
            crate::nifti_io::load_volume_f64(&ndir.join(names.efieldgauss_nii(Side::Right)))
                .unwrap();
        v.iter().sum::<f64>()
    };
    assert!((gauss - 1.0).abs() < 1e-4, "{gauss}");
    assert!(fx
        .sub
        .subj_dir
        .join("headmodel/native/sub-1_hemi-R_desc-efieldbasis.json")
        .is_file());

    let second = run(&fx, &opts);
    assert_eq!(token(&second), "ok:vat:0/1");
    assert!(second
        .lines
        .iter()
        .any(|l| l.contains("ef unchanged; kept")));

    s.sides.right.sources[0].amplitude = 2.5;
    save(&fx, &s);
    let third = run(&fx, &opts);
    assert_eq!(token(&third), "ok:vat:1/1", "{:?}", third.lines);
    let again = json_of(&ndir.join(names.vta_json()));
    assert_eq!(again["engine"]["basis_cache"], "hit");
    assert!(again["sides"]["right"]["volume_raw_mm3"].as_f64().unwrap() > 0.0);
}

#[test]
fn failures_carry_their_codes() {
    let fx = fixture("fail", true, true);
    let mut ff = stimulation("ff", ModelKind::Efield, Side::Right, Mode::Current, 2.0, 2);
    ff.model = ModelKind::Fastfield;
    save(&fx, &ff);
    let mut opts = options(&fx);
    opts.vat_labels = vec!["ff".to_owned()];
    let t = token(&run(&fx, &opts));
    assert!(
        t.contains("vat: E_VAT_model: ff:") && t.contains("fastfield"),
        "{t}"
    );

    save(
        &fx,
        &stimulation("gpu", ModelKind::Efield, Side::Right, Mode::Current, 2.0, 2),
    );
    opts.vat_labels = vec!["gpu".to_owned()];
    opts.vat_backend = VatBackend::Wgpu;
    let params = VatParams {
        device: no_device,
        ..small_params()
    };
    let t = token(&run_with(&fx, &opts, &params));
    assert!(
        t.contains("vat: E_VAT_capacity: gpu:") && t.contains("no adapter in this test"),
        "{t}"
    );

    let nn = fixture("nonative", false, true);
    save(
        &nn,
        &stimulation("nn", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    let t = token(&run(&nn, &options(&nn)));
    assert!(t.contains("vat: E_VAT_no_native: nn:"), "{t}");

    let norec = fixture("noreco", true, true);
    save(
        &norec,
        &stimulation("nr", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    std::fs::remove_file(norec.sub.reconstruction_json()).unwrap();
    let t = token(&run(&norec, &options(&norec)));
    assert_eq!(
        t,
        "error: stage prerequisite missing: vat: no reconstruction (run reconstruct)"
    );
}

#[test]
fn no_stimulation_settings_skip_and_an_unknown_label_fails() {
    let fx = fixture("none", true, true);
    assert_eq!(
        token(&run(&fx, &options(&fx))),
        "skip:vat: no stimulation settings"
    );
    let mut opts = options(&fx);
    opts.vat_labels = vec!["ghost".to_owned()];
    let t = token(&run(&fx, &opts));
    assert!(t.contains("E_VAT_settings") && t.contains("ghost"), "{t}");
}

#[test]
fn without_a_warp_the_outputs_are_native_only() {
    let fx = fixture("nowarp", true, false);
    save(
        &fx,
        &stimulation("nw", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2),
    );
    let ran = run(&fx, &options(&fx));
    assert_eq!(token(&ran), "ok:vat:1/1 (native only)", "{:?}", ran.lines);
    assert!(ran
        .lines
        .iter()
        .any(|l| l.contains("template-space outputs skipped")));
    let names = Names::new("1", "kuncel");
    assert!(label_dir_of(&fx, "native", "nw")
        .join(names.binary_nii(Side::Right))
        .is_file());
    assert!(!label_dir_of(&fx, SPACE, "nw").exists());
}

#[test]
fn a_lead_missing_from_the_reconstruction_skips_that_side_with_a_warning() {
    let fx = fixture("nolead", true, true);
    let reco_path = fx.sub.reconstruction_json();
    let mut reco = json_of(&reco_path);
    reco["native"].as_object_mut().unwrap().remove("2");
    std::fs::write(&reco_path, serde_json::to_string(&reco).unwrap()).unwrap();
    let mut s = stimulation("one", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2);
    let src = &mut s.sides.left.sources[0];
    src.mode = Mode::Voltage;
    src.amplitude = 3.0;
    src.contacts[1] = Contact {
        index: 2,
        polarity: Polarity::Cathode,
        percent: 100.0,
        impedance_kohm: 1.0,
    };
    save(&fx, &s);
    let ran = run(&fx, &options(&fx));
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    assert!(ran
        .lines
        .iter()
        .any(|l| l.contains("the left lead is not in the reconstruction")));
    let side =
        &json_of(&label_dir_of(&fx, "native", "one").join(Names::new("1", "kuncel").vta_json()))
            ["sides"];
    assert_eq!(side["left"]["status"], "no_lead");
}

#[test]
fn the_grey_matter_mask_is_resampled_to_native_through_the_inverse_warp() {
    let fx = fixture("gm", true, true);
    let warp = SpaceWarp::load(&fx.sub).unwrap();
    let mask = fx.root.join("gm.nii");
    let mut data = ndarray::Array3::<u8>::zeros((60, 60, 60));
    for k in 20..40usize {
        for j in 20..40usize {
            for i in 20..40usize {
                data[[i, j, k]] = 1;
            }
        }
    }
    let aff = [
        [1.0, 0.0, 0.0, -10.0],
        [0.0, 1.0, 0.0, -30.0],
        [0.0, 0.0, 1.0, -30.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    crate::nifti_io::write_u8_3d(&mask, &data, &aff).unwrap();
    let lead = find_lead(
        &json_of(&fx.sub.reconstruction_json()),
        &options(&fx),
        Side::Right,
    )
    .unwrap()
    .unwrap();
    let vol = gm_volume(&mask, &warp, &lead, 1.0).unwrap();
    let inside_template = |t: [f64; 3]| {
        (0..3).all(|a| t[a] >= [10.0, -10.0, -10.0][a] && t[a] <= [29.0, 9.0, 9.0][a])
    };
    let mut checked = 0;
    for p in [
        [10.0, -8.0, -8.0],
        [8.0, -8.0, -8.0],
        [5.0, -8.0, -8.0],
        [30.0, -8.0, -8.0],
    ] {
        let t = [p[0] + SHIFT[0], p[1] + SHIFT[1], p[2] + SHIFT[2]];
        let v = vol.sample_nearest(p).unwrap_or(0.0);
        assert_eq!(v > 0.5, inside_template(t), "{p:?} -> {t:?}: {v}");
        checked += 1;
    }
    assert_eq!(checked, 4);
}

#[test]
fn the_up_to_date_check_follows_the_settings_and_the_stats_atlas() {
    let fx = fixture("uptodate", true, true);
    let s = stimulation("up", ModelKind::Kuncel, Side::Right, Mode::Voltage, 3.0, 2);
    save(&fx, &s);
    let opts = options(&fx);
    assert_eq!(token(&run(&fx, &opts)), "ok:vat:1/1");
    assert_eq!(token(&run(&fx, &opts)), "ok:vat:0/1");
    let mut other = opts.clone();
    other.vat_stats_atlas = Some("external:Other".to_owned());
    let t = token(&run(&fx, &other));
    assert_eq!(t, "ok:vat:1/1");
    let mut forced = opts.clone();
    forced.overwrite_approved = true;
    assert_eq!(token(&run(&fx, &forced)), "ok:vat:1/1");
}

#[test]
fn a_preview_returns_the_native_field_and_writes_no_result_files() {
    let fx = fixture("preview", true, true);
    let mut s = stimulation("pv", ModelKind::Efield, Side::Right, Mode::Current, 2.0, 2);
    let sub = resolve_subject_with(&fx.root, "1", false).unwrap();
    let opts = options(&fx);
    let p = crate::vat::preview::preview(&sub, &opts, &s, &small_params(), true, None).unwrap();
    assert_eq!(p.sides.len(), 1);
    let (side, out) = &p.sides[0];
    assert_eq!(*side, Side::Right);
    assert!(out.field.volumes.raw_mm3 > 0.0);
    assert_eq!(out.basis_cache, "built");
    assert!(!sub.subj_dir.join("stimulations").exists());
    s.sides.right.sources[0].amplitude = 3.0;
    let q = crate::vat::preview::preview(&sub, &opts, &s, &small_params(), false, None).unwrap();
    assert_eq!(q.sides[0].1.basis_cache, "hit");
    assert!(q.sides[0].1.field.volumes.raw_mm3 > out.field.volumes.raw_mm3);
    s.model = ModelKind::Ossdbs;
    let e = crate::vat::preview::preview(&sub, &opts, &s, &small_params(), false, None);
    assert!(matches!(
        e,
        Err(Fail::Code {
            code: "E_VAT_model",
            ..
        })
    ));
}

fn efield_fixture_run(name: &str, backend: VatBackend, params: &VatParams) -> (Fixture, Ran) {
    let fx = fixture(name, true, true);
    save(
        &fx,
        &stimulation("ef", ModelKind::Efield, Side::Right, Mode::Current, 2.0, 2),
    );
    let mut opts = options(&fx);
    opts.vat_backend = backend;
    let ran = run_with(&fx, &opts, params);
    (fx, ran)
}

fn require_adapter() -> bool {
    std::env::var("DBSLOC_GPU_REQUIRE_ADAPTER")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn software_allowed() -> bool {
    fastwarp_gpu::device::env_var("FASTWARP_GPU_ALLOW_SOFTWARE")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

#[test]
fn auto_without_a_device_uses_the_cpu_engine_and_says_so() {
    let params = VatParams {
        device: no_device,
        ..small_params()
    };
    let (fx, ran) = efield_fixture_run("auto_nodev", VatBackend::Auto, &params);
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    assert!(
        ran.lines
            .iter()
            .any(|l| l.starts_with("dbsloc: note: vat: ")
                && l.contains("no adapter in this test")
                && l.ends_with("; using the CPU engine")),
        "{:?}",
        ran.lines
    );
    let names = Names::new("1", "simbio");
    let sidecar = json_of(&label_dir_of(&fx, "native", "ef").join(names.vta_json()));
    assert_eq!(sidecar["engine"]["backend"], "cpu");
    assert!(sidecar["engine"]["adapter"].is_null());
    let cache = json_of(
        &fx.sub
            .subj_dir
            .join("headmodel/native/sub-1_hemi-R_desc-efieldbasis.json"),
    );
    assert_eq!(cache["built_by"], "cpu");
}

#[test]
fn explicit_cpu_never_asks_for_the_device() {
    fn forbidden() -> Result<std::sync::Arc<fastwarp_gpu::WgpuBackend>, String> {
        panic!("the device must not be requested");
    }
    let params = VatParams {
        device: forbidden,
        ..small_params()
    };
    let (_fx, ran) = efield_fixture_run("cpu_only", VatBackend::Cpu, &params);
    assert_eq!(token(&ran), "ok:vat:1/1", "{:?}", ran.lines);
    assert!(!ran.lines.iter().any(|l| l.contains("dbsloc: note: vat:")));
}

#[test]
fn the_wgpu_engine_matches_the_cpu_field_and_records_its_adapter() {
    if crate::reg_backend::shared_backend().is_err() {
        assert!(
            !(require_adapter() && software_allowed()),
            "no GPU adapter with DBSLOC_GPU_REQUIRE_ADAPTER and DBSLOC_GPU_ALLOW_SOFTWARE set"
        );
        return;
    }
    let cpu_t = std::time::Instant::now();
    let (cpu_fx, cpu) = efield_fixture_run("par_cpu", VatBackend::Cpu, &small_params());
    let cpu_s = cpu_t.elapsed().as_secs_f64();
    let gpu_t = std::time::Instant::now();
    let (gpu_fx, gpu) = efield_fixture_run("par_gpu", VatBackend::Wgpu, &small_params());
    let gpu_s = gpu_t.elapsed().as_secs_f64();
    assert_eq!(token(&cpu), "ok:vat:1/1", "{:?}", cpu.lines);
    assert_eq!(token(&gpu), "ok:vat:1/1", "{:?}", gpu.lines);
    let names = Names::new("1", "simbio");
    let side = |fx: &Fixture| {
        let dir = label_dir_of(fx, "native", "ef");
        (
            json_of(&dir.join(names.vta_json())),
            crate::nifti_io::load_volume_f64(&dir.join(names.efield_nii(Side::Right)))
                .unwrap()
                .0,
        )
    };
    let (cpu_json, cpu_e) = side(&cpu_fx);
    let (gpu_json, gpu_e) = side(&gpu_fx);
    assert_eq!(cpu_json["engine"]["backend"], "cpu");
    assert_eq!(gpu_json["engine"]["backend"], "wgpu");
    assert!(gpu_json["engine"]["adapter"]
        .as_str()
        .is_some_and(|a| !a.is_empty()));
    assert!(gpu_json["engine"]["relative_residual"].as_f64().unwrap() > 0.0);
    assert_eq!(cpu_e.dim(), gpu_e.dim());
    let (mut diff, mut norm) = (0.0f64, 0.0f64);
    for (a, b) in cpu_e.iter().zip(gpu_e.iter()) {
        diff += (a - b) * (a - b);
        norm += a * a;
    }
    assert!(norm > 0.0);
    assert!(
        (diff / norm).sqrt() < 0.01,
        "relative L2 {}",
        (diff / norm).sqrt()
    );
    let cache = json_of(
        &gpu_fx
            .sub
            .subj_dir
            .join("headmodel/native/sub-1_hemi-R_desc-efieldbasis.json"),
    );
    assert_eq!(cache["built_by"], "wgpu");
    eprintln!(
        "vat efield stage: adapter {}, cpu {cpu_s:.2} s, wgpu {gpu_s:.2} s",
        gpu_json["engine"]["adapter"]
    );
}
