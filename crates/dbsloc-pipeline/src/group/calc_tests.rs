#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::cell::Cell;

use dbsloc_reconstruct::electrode_by_name;
use dbsloc_vat::geometry::GridSpec;
use fastwarp_reg::transform::itk_h5::write_h5_file;
use fastwarp_reg::transform::{AffineTransform, TransformChain, TransformElem};
use serde_json::json;

use super::*;
use crate::bids::resolve_subject_with;
use crate::group::ea_stats::read_ea_stats;
use crate::vat::settings::{self as cfg, CaseSpec, Contact, Mode as StimMode, Polarity};

const SPACE: &str = "MNI152NLin2009bAsym";
const MODEL: &str = "Medtronic 3389";
const GUID: &str = "20261023101500";
const SHIFT: [f64; 3] = [3.0, -2.0, 4.0];
const HEAD_R: [f64; 3] = [10.0, -12.0, -8.0];
const HEAD_L: [f64; 3] = [-10.0, -12.0, -8.0];

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    atlases: PathBuf,
    folder: PathBuf,
    record: GroupRecord,
}

fn label() -> String {
    format!("gs_{GUID}")
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

fn write_atlas(dir: &Path) {
    let set = dir.join("Synth").join("rh");
    std::fs::create_dir_all(&set).unwrap();
    let mut data = ndarray::Array3::<u8>::zeros((30, 30, 30));
    data.fill(1);
    let aff = [
        [1.0, 0.0, 0.0, 5.0],
        [0.0, 1.0, 0.0, -30.0],
        [0.0, 0.0, 1.0, -30.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    crate::nifti_io::write_u8_3d(&set.join("cube.nii"), &data, &aff).unwrap();
}

fn make_subject(root: &Path, id: &str) {
    let anat = root.join(format!("rawdata/sub-{id}/ses-preop/anat"));
    std::fs::create_dir_all(&anat).unwrap();
    std::fs::write(anat.join(format!("sub-{id}_ses-preop_T1w.nii.gz")), b"x").unwrap();
    let probe = resolve_subject_with(root, id, false).unwrap();
    std::fs::create_dir_all(&probe.recon_dir).unwrap();
    let reco = json!({
        "version": 2,
        "subj_id": id,
        "props": {"1": {"elmodel": MODEL}, "2": {"elmodel": MODEL}},
        "meta": {"side_keys": "1=right,2=left"},
        "native": {"1": {"markers": markers(HEAD_R)}, "2": {"markers": markers(HEAD_L)}},
        "mni": {"1": {"coords_mm": [[13.0, -14.0, -4.0], [13.0, -14.0, -2.0], [40.0, -14.0, -4.0]]}},
    });
    std::fs::write(
        probe.reconstruction_json(),
        serde_json::to_string_pretty(&reco).unwrap(),
    )
    .unwrap();
    normalize_into(root, id, SPACE);
}

fn normalize_into(root: &Path, id: &str, space: &str) {
    let probe = resolve_subject_with(root, id, false).unwrap();
    std::fs::create_dir_all(crate::derivatives::norm_log_dir(&probe)).unwrap();
    std::fs::write(
        crate::derivatives::norm_method_log(&probe),
        format!(r#"{{"template_space": "{space}"}}"#),
    )
    .unwrap();
    std::fs::create_dir_all(crate::derivatives::norm_transform_dir(&probe)).unwrap();
    let forward = AffineTransform::from_translation([SHIFT[0], SHIFT[1], -SHIFT[2]]);
    let inverse = forward.inverse().unwrap();
    write_h5_file(
        &crate::derivatives::norm_forward(&probe, space),
        &TransformChain::from_queue(vec![TransformElem::Affine(forward)]),
    )
    .unwrap();
    write_h5_file(
        &crate::derivatives::norm_inverse(&probe, space),
        &TransformChain::from_queue(vec![TransformElem::Affine(inverse)]),
    )
    .unwrap();
}

fn stimulation(label: &str, id: &str, model: ModelKind) -> Stimulation {
    let mut s = Stimulation::blank(label, id, model, [4, 4]);
    let src = &mut s.sides.get_mut(Side::Right).sources[0];
    src.mode = StimMode::Voltage;
    src.amplitude = 3.0;
    src.contacts[1] = Contact {
        index: 2,
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

fn subject_dir(root: &Path, id: &str) -> PathBuf {
    resolve_subject_with(root, id, false).unwrap().subj_dir
}

fn fixture(name: &str, ids: &[&str]) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(&format!("dbsloc_calc_{name}_"))
        .tempdir()
        .unwrap();
    let root = dir.path().join("Cohort");
    let atlases = dir.path().join("atlases");
    write_atlas(&atlases);
    let mut record = GroupRecord::new(GUID, "Cohort");
    record.atlas_set = Some("external:Synth".to_owned());
    for id in ids {
        make_subject(&root, id);
        let sd = subject_dir(&root, id);
        cfg::save(&sd, &stimulation(&label(), id, ModelKind::Kuncel)).unwrap();
        let deriv = sd.parent().unwrap().to_string_lossy().into_owned();
        let d = record.ensure_dataset(&root.to_string_lossy(), &deriv);
        record.add_patient(&format!("sub-{id}"), &d, 1).unwrap();
    }
    let folder = root.join("derivatives").join("leadgroup").join(GUID);
    std::fs::create_dir_all(&folder).unwrap();
    Fixture {
        _dir: dir,
        root,
        atlases,
        folder,
        record,
    }
}

fn options(fx: &Fixture) -> CalcOptions {
    let mut run = RunOptions::new(&fx.root, "");
    run.use_index = false;
    run.el_model = None;
    run.leaddbs_atlases_dir = Some(fx.atlases.clone());
    let mut o = CalcOptions::new(run);
    o.params = VatParams {
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
    };
    o
}

fn calculate(fx: &mut Fixture, o: &CalcOptions, cancel: Option<&AtomicBool>) -> CalcReport {
    run_calculate(&mut fx.record, &fx.folder, o, cancel, &mut |_| {}).unwrap()
}

fn phases(r: &CalcReport) -> Vec<CalcPhase> {
    r.outcomes.iter().map(|o| o.phase).collect()
}

#[test]
fn two_subjects_are_calculated_and_a_second_run_keeps_everything() {
    let mut fx = fixture("two", &["1", "2"]);
    let o = options(&fx);
    let first = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&first),
        vec![CalcPhase::Computed, CalcPhase::Computed],
        "{:?}",
        first.outcomes
    );
    assert_eq!(first.exit_code(), 0);
    for id in ["1", "2"] {
        let sd = subject_dir(&fx.root, id);
        for space in ["native", SPACE] {
            let dir = label_dir(&sd, space, &label());
            assert!(
                dir.join(format!("sub-{id}_sim-binary_model-kuncel_hemi-R.nii"))
                    .is_file(),
                "{space}"
            );
        }
        let stats_json =
            label_dir(&sd, SPACE, &label()).join(format!("sub-{id}_desc-vtastats.json"));
        assert!(stats_json.is_file());
        let read = read_ea_stats(&stats_path(&sd, &format!("sub-{id}"))).unwrap();
        assert!(read.stats.stimulation_of(&label()).is_some());
        let right = &read.stats.conmat[0][0];
        assert_eq!(right.len(), 3);
        assert!((right[0][0]).abs() < 1e-9);
        assert!((right[2][0] - 6.0).abs() < 1e-9, "{right:?}");
        assert!(read.stats.conmat[0][1].is_empty());
        let hull = &read.stats.conmat_inside_hull[0][0];
        assert_eq!(hull[0][0], 0.0);
        assert!((hull[2][0] - 6.0).abs() < 1e-9);
    }
    assert!(first
        .outcomes
        .iter()
        .all(|o| o.distances == DistanceState::Computed));
    let fingerprint = first.fingerprint.clone().unwrap();
    assert_eq!(fingerprint.len(), 64);
    let analysis: Value = serde_json::from_str(
        &std::fs::read_to_string(first.analysis_record.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(analysis["fingerprint"], json!(fingerprint));
    let inputs = analysis["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 2);
    for i in inputs {
        for key in [
            "reconstruction_sha256",
            "stimulation_sha256",
            "vtastats_sha256",
        ] {
            assert!(i[key].is_string(), "{key}");
        }
    }
    assert_eq!(fx.record.analyses.len(), 1);
    assert_eq!(fx.record.analyses[0].kind, ANALYSIS_KIND);
    assert_eq!(
        fx.record.stimulation.as_ref().unwrap().model,
        Some(ModelKind::Kuncel)
    );
    assert_eq!(calc_is_stale(&fx.record, &fx.folder), Some(false));

    let second = calculate(&mut fx, &o, None);
    assert_eq!(phases(&second), vec![CalcPhase::Kept, CalcPhase::Kept]);
    assert!(second
        .outcomes
        .iter()
        .all(|o| o.distances == DistanceState::Kept));
    assert_eq!(second.fingerprint.unwrap(), fingerprint);
    assert_eq!(fx.record.analyses.len(), 1);

    let sd = subject_dir(&fx.root, "1");
    let mut changed = stimulation(&label(), "1", ModelKind::Kuncel);
    changed.sides.get_mut(Side::Right).sources[0].amplitude = 2.0;
    cfg::save(&sd, &changed).unwrap();
    assert_eq!(calc_is_stale(&fx.record, &fx.folder), Some(true));
    let third = calculate(&mut fx, &o, None);
    assert_eq!(phases(&third), vec![CalcPhase::Computed, CalcPhase::Kept]);
    assert_ne!(third.fingerprint.unwrap(), fingerprint);
}

#[test]
fn a_cancel_stops_after_the_current_subject() {
    let mut fx = fixture("cancel", &["1", "2"]);
    let o = options(&fx);
    let flag = AtomicBool::new(false);
    let report = run_calculate(&mut fx.record, &fx.folder, &o, Some(&flag), &mut |ev| {
        if matches!(ev, CalcEvent::SubjectFinished(_)) {
            flag.store(true, Ordering::SeqCst);
        }
    })
    .unwrap();
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed, CalcPhase::Cancelled]
    );
    assert_eq!(report.exit_code(), 130);
    assert!(report.analysis_record.is_none());
    assert!(!label_dir(&subject_dir(&fx.root, "2"), SPACE, &label()).exists());
}

#[test]
fn a_subject_without_reconstruction_is_reported_and_the_others_complete() {
    let mut fx = fixture("noreco", &["1", "2", "3"]);
    let sd2 = subject_dir(&fx.root, "2");
    std::fs::remove_file(reco_path(&sd2, "sub-2")).unwrap();
    let o = options(&fx);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed, CalcPhase::NotRun, CalcPhase::Computed]
    );
    assert_eq!(report.exit_code(), 1);
    assert_eq!(
        report.outcomes[1].message.as_deref(),
        Some("no reconstruction")
    );
    assert!(label_dir(&subject_dir(&fx.root, "3"), SPACE, &label()).is_dir());
    let json = report.to_json();
    assert_eq!(json["totals"]["failed"], 1);
    assert_eq!(json["subjects"][1]["status"], "not_run");

    let mut o = options(&fx);
    o.stop_on_first_failure = true;
    o.subjects = Some(vec!["2".to_owned(), "3".to_owned()]);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::NotRun, CalcPhase::Cancelled]
    );
    assert!(report.outcomes[1]
        .message
        .as_deref()
        .unwrap()
        .contains("stopped after sub-2"));
}

#[test]
fn a_different_vta_model_is_refused_per_subject() {
    let mut fx = fixture("mix", &["1", "2"]);
    let sd2 = subject_dir(&fx.root, "2");
    cfg::save(&sd2, &stimulation(&label(), "2", ModelKind::Maedler)).unwrap();
    let o = options(&fx);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed, CalcPhase::Failed]
    );
    assert_eq!(
        report.outcomes[1].code.as_deref(),
        Some("E_GROUP_MODEL_MIX")
    );
    assert_eq!(report.exit_code(), 1);
}

#[test]
fn missing_settings_are_reported_or_copied_from_another_label() {
    let mut fx = fixture("copy", &["1"]);
    let sd = subject_dir(&fx.root, "1");
    let mut other = stimulation("k3v", "1", ModelKind::Kuncel);
    other.sides.get_mut(Side::Right).sources[0].amplitude = 2.5;
    cfg::save(&sd, &other).unwrap();
    std::fs::remove_dir_all(label_dir(&sd, "native", &label())).unwrap();
    let o = options(&fx);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(phases(&report), vec![CalcPhase::NotRun]);
    assert_eq!(report.outcomes[0].code.as_deref(), Some("E_VAT_settings"));

    let mut o = options(&fx);
    o.copy_settings_from = Some("k3v".to_owned());
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    let copied = cfg::load(&sd, "1", &label()).unwrap();
    assert_eq!(copied.label, label());
    assert_eq!(copied.sides.get(Side::Right).sources[0].amplitude, 2.5);
}

#[test]
fn another_atlas_set_backs_up_the_statistics_and_drops_old_entries() {
    let mut fx = fixture("backup", &["1"]);
    let sd = subject_dir(&fx.root, "1");
    let old = EaStats {
        atlas_names: vec!["other.nii".to_owned()],
        atlas_types: vec![3.0],
        stimulation: vec![crate::group::ea_stats::StimulationStats {
            label: "gs_old".to_owned(),
            ..Default::default()
        }],
        ..EaStats::default()
    };
    let path = stats_path(&sd, "sub-1");
    std::fs::write(
        &path,
        mat5_bytes(&[(EA_STATS_VARIABLE.to_owned(), old.to_mat())], true).unwrap(),
    )
    .unwrap();
    let o = options(&fx);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(phases(&report), vec![CalcPhase::Computed]);
    assert!(report.outcomes[0].backed_up_stats);
    assert!(sd.join("sub-1_desc-stats_backup.mat").is_file());
    let now = read_ea_stats(&path).unwrap().stats;
    assert_eq!(now.atlas_names, vec!["cube.nii".to_owned()]);
    assert!(now.stimulation_of("gs_old").is_none());
    assert!(now.stimulation_of(&label()).is_some());
}

#[test]
fn mirroring_writes_the_flipped_files_beside_the_originals() {
    let mut fx = fixture("mirror", &["1"]);
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_transform = FlipTransformChoice::PlainFlip;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    assert!(report
        .notes
        .iter()
        .any(|n| n.to_string().contains("plain x flip")));
    let dir = label_dir(&subject_dir(&fx.root, "1"), SPACE, &label());
    assert!(dir
        .join("sub-1_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii")
        .is_file());
    assert!(!report.outcomes[0].mirrored.is_empty());

    o.flip_transform = FlipTransformChoice::Path(PathBuf::from("flip.h5"));
    let report = calculate(&mut fx, &o, None);
    assert_eq!(phases(&report), vec![CalcPhase::Failed]);
    assert_eq!(report.outcomes[0].code.as_deref(), Some("E_GROUP_FLIP"));
}

fn flipped_in(fx: &Fixture) -> Vec<String> {
    flipped_in_space(fx, SPACE)
}

fn flipped_in_space(fx: &Fixture, space: &str) -> Vec<String> {
    let dir = label_dir(&subject_dir(&fx.root, "1"), space, &label());
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("_hemidesc-FlippedFrom"))
        .collect();
    v.sort();
    v
}

fn mirrored_fixture(name: &str) -> (Fixture, CalcOptions) {
    let mut fx = fixture(name, &["1"]);
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_transform = FlipTransformChoice::PlainFlip;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(phases(&report), vec![CalcPhase::Computed]);
    assert!(!flipped_in(&fx).is_empty());
    (fx, o)
}

fn change_the_group_settings(fx: &Fixture) {
    let mut s = stimulation(&label(), "1", ModelKind::Kuncel);
    s.sides.get_mut(Side::Right).sources[0].amplitude = 2.0;
    cfg::save(&subject_dir(&fx.root, "1"), &s).unwrap();
}

#[test]
fn calculate_without_mirroring_removes_the_mirrors_of_recomputed_originals() {
    let (mut fx, mut o) = mirrored_fixture("unmirror");
    change_the_group_settings(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    assert!(flipped_in(&fx).is_empty(), "{:?}", flipped_in(&fx));
    assert!(report.notes.iter().any(|n| n
        .to_string()
        .contains("were removed because the originals were rewritten")));
}

#[test]
fn a_mirroring_run_that_fails_at_the_flip_transform_leaves_no_old_mirrors() {
    use super::super::fliplr::{FlipBuildError, BUILD_HOOK};

    let (mut fx, mut o) = mirrored_fixture("flipfail_after");
    change_the_group_settings(&fx);
    o.flip_transform = FlipTransformChoice::Auto;
    o.flip_cache_root = Some(fx._dir.path().join("userdata"));
    BUILD_HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(|_, _| {
            Err(FlipBuildError::Failed("no engine".into()))
        }));
    });
    let report = calculate(&mut fx, &o, None);
    BUILD_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(phases(&report), vec![CalcPhase::Failed]);
    assert_eq!(report.outcomes[0].code.as_deref(), Some("E_GROUP_FLIP"));
    assert!(flipped_in(&fx).is_empty(), "{:?}", flipped_in(&fx));
    assert!(has_removal_note(&report), "{:?}", report.notes);
}

fn has_removal_note(report: &CalcReport) -> bool {
    report.notes.iter().any(|n| {
        n.to_string()
            .contains("were removed because the originals were rewritten")
    })
}

fn template_dir(fx: &Fixture) -> PathBuf {
    label_dir(&subject_dir(&fx.root, "1"), SPACE, &label())
}

#[test]
fn a_flipped_file_that_cannot_be_deleted_fails_the_patient() {
    let (mut fx, mut o) = mirrored_fixture("stuckmirror");
    let before = flipped_in(&fx);
    let stuck = template_dir(&fx).join(&before[0]);
    std::fs::remove_file(&stuck).unwrap();
    std::fs::create_dir(&stuck).unwrap();
    std::fs::write(stuck.join("held"), b"x").unwrap();
    std::fs::write(
        template_dir(&fx).join("sub-1_sim-binary_model-kuncel_hemi-R_hemidesc-FlippedFromLeft.nii"),
        b"x",
    )
    .unwrap();
    assert_eq!(flipped_in(&fx).len(), 2);
    change_the_group_settings(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Failed],
        "{:?}",
        report.outcomes
    );
    assert_eq!(report.outcomes[0].code.as_deref(), Some("E_GROUP_FLIP"));
    let m = report.outcomes[0].message.as_deref().unwrap();
    assert!(
        m.contains(&before[0]) && m.contains("could not be deleted"),
        "{m}"
    );
    assert_eq!(flipped_in(&fx), vec![before[0].clone()]);
    assert!(has_removal_note(&report), "{:?}", report.notes);
}

#[test]
fn a_native_only_recompute_keeps_the_mirrors() {
    let (mut fx, mut o) = mirrored_fixture("nativeonly");
    let before = flipped_in(&fx);
    let probe = resolve_subject_with(&fx.root, "1", false).unwrap();
    std::fs::remove_file(crate::derivatives::norm_forward(&probe, SPACE)).unwrap();
    std::fs::remove_file(crate::derivatives::norm_inverse(&probe, SPACE)).unwrap();
    change_the_group_settings(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    assert_eq!(flipped_in(&fx), before);
    assert!(!has_removal_note(&report), "{:?}", report.notes);
}

#[test]
fn a_vat_commit_that_fails_after_a_template_file_removes_the_mirrors() {
    let (mut fx, mut o) = mirrored_fixture("commitfail");
    let blocker = template_dir(&fx).join("sub-1_desc-vtastats.json");
    assert!(blocker.is_file());
    std::fs::remove_file(&blocker).unwrap();
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("held"), b"x").unwrap();
    change_the_group_settings(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Failed],
        "{:?}",
        report.outcomes
    );
    assert!(flipped_in(&fx).is_empty(), "{:?}", flipped_in(&fx));
    assert!(has_removal_note(&report), "{:?}", report.notes);
}

#[test]
fn a_mirroring_rerun_replaces_the_mirrors_without_a_note() {
    let (mut fx, o) = mirrored_fixture("remirror");
    let before = flipped_in(&fx);
    let orphan =
        template_dir(&fx).join("sub-1_sim-binary_model-kuncel_hemi-R_hemidesc-FlippedFromLeft.nii");
    std::fs::write(&orphan, b"x").unwrap();
    change_the_group_settings(&fx);
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    assert!(!orphan.exists());
    assert_eq!(flipped_in(&fx), before);
    assert!(!report.outcomes[0].mirrored.is_empty());
    assert!(!has_removal_note(&report), "{:?}", report.notes);
}

#[test]
fn a_vat_step_that_fails_before_its_commit_keeps_the_mirrors() {
    const OTHER: &str = "MNI152NLin2009cAsym";
    let mut fx = fixture("precommit", &["1"]);
    let probe = resolve_subject_with(&fx.root, "1", false).unwrap();
    std::fs::remove_file(crate::derivatives::norm_forward(&probe, SPACE)).unwrap();
    std::fs::remove_file(crate::derivatives::norm_inverse(&probe, SPACE)).unwrap();
    normalize_into(&fx.root, "1", OTHER);
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_transform = FlipTransformChoice::PlainFlip;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    let before = flipped_in_space(&fx, OTHER);
    assert!(!before.is_empty());
    let reference = label_dir(&subject_dir(&fx.root, "1"), SPACE, &label());
    assert!(reference.join("sub-1_desc-stimparameters.mat").is_file());
    let blocker = reference.join(".partial-.partial-sub-1_desc-stimparameters.mat");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("held"), b"x").unwrap();
    let planted =
        reference.join("sub-1_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii");
    std::fs::write(&planted, b"x").unwrap();
    let reference_before = flipped_in_space(&fx, SPACE);
    assert!(!reference_before.is_empty());
    change_the_group_settings(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Failed],
        "{:?}",
        report.outcomes
    );
    let m = report.outcomes[0].message.as_deref().unwrap();
    assert!(m.contains("stimparameters"), "{m}");
    assert_eq!(flipped_in_space(&fx, OTHER), before);
    assert_eq!(flipped_in_space(&fx, SPACE), reference_before);
    assert!(!has_removal_note(&report), "{:?}", report.notes);
}

#[test]
fn a_cancelled_vat_with_an_undeletable_mirror_stays_cancelled() {
    let fx = fixture("stuckcancel", &["1"]);
    let o = options(&fx);
    let flips = RefCell::new(HashMap::new());
    let job = Job {
        record: &fx.record,
        options: &o,
        mode: Mode::Calculate,
        atlas: None,
        cancel: None,
        flips: &flips,
    };
    let prepared = Prepared {
        stim: Err((None, "unused".to_owned())),
    };
    let stuck = template_dir(&fx)
        .join("sub-1_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii");
    let stale = FlippedRemoval {
        removed: Vec::new(),
        failed: vec![(stuck.clone(), "in use".to_owned())],
    };
    let mut notes = Vec::new();
    let outcome = finish_subject(
        &job,
        &prepared,
        "sub-1",
        &subject_dir(&fx.root, "1"),
        &label(),
        SubjectOutcome::new("sub-1", CalcPhase::Kept),
        VatResult::Cancelled,
        &stale,
        &mut notes,
        &mut |_| {},
    );
    assert_eq!(outcome.phase, CalcPhase::Cancelled);
    assert_eq!(outcome.code.as_deref(), Some("E_GROUP_FLIP"));
    let m = outcome.message.as_deref().unwrap();
    assert!(m.contains(&stuck.display().to_string()), "{m}");
    assert!(m.contains("the vat step was cancelled"), "{m}");
}

#[test]
fn a_run_that_recomputes_nothing_keeps_the_mirrors() {
    let (mut fx, mut o) = mirrored_fixture("keepmirror");
    let before = flipped_in(&fx);
    o.mirror = false;
    let report = calculate(&mut fx, &o, None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Kept],
        "{:?}",
        report.outcomes
    );
    assert_eq!(flipped_in(&fx), before);
}

#[test]
fn mirroring_builds_a_missing_flip_transform_once() {
    use super::super::fliplr::{forward_name, inverse_name, BUILD_HOOK};
    use std::rc::Rc;

    let mut fx = fixture("autoflip", &["1", "2"]);
    let cache = fx._dir.path().join("userdata");
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_cache_root = Some(cache.clone());
    let builds = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&builds);
    BUILD_HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |space, dir| {
            counter.set(counter.get() + 1);
            std::fs::create_dir_all(dir).unwrap();
            let identity =
                TransformChain::from_queue(vec![
                    TransformElem::Affine(AffineTransform::identity()),
                ]);
            write_h5_file(&dir.join(forward_name(space)), &identity).unwrap();
            write_h5_file(&dir.join(inverse_name(space)), &identity).unwrap();
            Ok(())
        }));
    });
    let report = calculate(&mut fx, &o, None);
    BUILD_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed, CalcPhase::Computed],
        "{:?}",
        report.outcomes
    );
    assert_eq!(builds.get(), 1);
    assert!(cache
        .join("templates/space")
        .join(SPACE)
        .join("fliplr")
        .join(forward_name(SPACE))
        .is_file());
    for outcome in &report.outcomes {
        assert!(!outcome.mirrored.is_empty());
    }
    let dir = label_dir(&subject_dir(&fx.root, "2"), SPACE, &label());
    assert!(dir
        .join("sub-2_sim-binary_model-kuncel_hemi-L_hemidesc-FlippedFromRight.nii")
        .is_file());
}

#[test]
fn a_failed_flip_build_fails_the_subject_with_the_hint() {
    use super::super::fliplr::{FlipBuildError, BUILD_HOOK};

    let mut fx = fixture("flipfail", &["1"]);
    let mut o = options(&fx);
    o.mirror = true;
    o.flip_cache_root = Some(fx._dir.path().join("userdata"));
    BUILD_HOOK.with(|h| {
        *h.borrow_mut() = Some(Box::new(|_, _| {
            Err(FlipBuildError::Failed("no engine".into()))
        }));
    });
    let report = calculate(&mut fx, &o, None);
    BUILD_HOOK.with(|h| *h.borrow_mut() = None);
    assert_eq!(phases(&report), vec![CalcPhase::Failed]);
    let o = &report.outcomes[0];
    assert_eq!(o.code.as_deref(), Some("E_GROUP_FLIP"));
    let m = o.message.as_deref().unwrap();
    assert!(m.contains("no engine") && m.contains("--plain-flip"), "{m}");
}

#[test]
fn the_distance_pass_alone_writes_no_vta() {
    let mut fx = fixture("dist", &["1", "2"]);
    let o = options(&fx);
    let events = Cell::new(0usize);
    let report = run_contact_distances(&mut fx.record, &fx.folder, &o, None, &mut |ev| {
        if matches!(ev, CalcEvent::SubjectStarted { .. }) {
            events.set(events.get() + 1);
        }
    })
    .unwrap();
    assert_eq!(events.get(), 2);
    assert_eq!(
        phases(&report),
        vec![CalcPhase::Computed, CalcPhase::Computed]
    );
    let sd = subject_dir(&fx.root, "1");
    assert!(stats_path(&sd, "sub-1").is_file());
    assert!(!label_dir(&sd, SPACE, &label()).exists());
    assert!(fx.record.analyses.is_empty());
    let again = run_contact_distances(&mut fx.record, &fx.folder, &o, None, &mut |_| {}).unwrap();
    assert_eq!(phases(&again), vec![CalcPhase::Kept, CalcPhase::Kept]);
}

#[test]
fn an_unknown_selection_is_an_error() {
    let mut fx = fixture("select", &["1"]);
    let mut o = options(&fx);
    o.subjects = Some(vec!["9".to_owned()]);
    let err = run_calculate(&mut fx.record, &fx.folder, &o, None, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("not in the group"));
}

fn gs_file(sd: &Path) -> PathBuf {
    label_dir(sd, "native", &label()).join("sub-1_desc-stimulation.json")
}

fn sync_fixture(name: &str) -> (Fixture, PathBuf) {
    let fx = fixture(name, &["1"]);
    let sd = subject_dir(&fx.root, "1");
    let mut other = stimulation("k3v", "1", ModelKind::Kuncel);
    other.sides.get_mut(Side::Right).sources[0].amplitude = 2.5;
    cfg::save(&sd, &other).unwrap();
    (fx, sd)
}

fn prepare(fx: &Fixture, o: &CalcOptions) -> Result<Stimulation, (Option<&'static str>, String)> {
    prepare_settings(&fx.record, &fx.record.patients[0], o)
}

#[test]
fn sync_replaces_a_stale_group_copy_after_the_source_changed() {
    let (fx, sd) = sync_fixture("sync_stale");
    let mut o = options(&fx);
    o.copy_settings_from = Some("k3v".to_owned());
    o.sync_settings = true;
    prepare(&fx, &o).unwrap();
    assert_eq!(
        cfg::load(&sd, "1", &label())
            .unwrap()
            .sides
            .get(Side::Right)
            .sources[0]
            .amplitude,
        2.5
    );
    let mut changed = cfg::load(&sd, "1", "k3v").unwrap();
    changed.sides.get_mut(Side::Right).sources[0].amplitude = 3.5;
    cfg::save(&sd, &changed).unwrap();
    let got = prepare(&fx, &o).unwrap();
    assert_eq!(got.sides.get(Side::Right).sources[0].amplitude, 3.5);
    let stored = cfg::load(&sd, "1", &label()).unwrap();
    assert_eq!(stored.label, label());
    assert_eq!(stored.sides.get(Side::Right).sources[0].amplitude, 3.5);
}

#[test]
fn sync_does_not_rewrite_an_unchanged_group_copy() {
    let (fx, sd) = sync_fixture("sync_same");
    let mut o = options(&fx);
    o.copy_settings_from = Some("k3v".to_owned());
    o.sync_settings = true;
    prepare(&fx, &o).unwrap();
    let before = std::fs::read(gs_file(&sd)).unwrap();
    let mtime = std::fs::metadata(gs_file(&sd)).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    prepare(&fx, &o).unwrap();
    assert_eq!(std::fs::read(gs_file(&sd)).unwrap(), before);
    assert_eq!(
        std::fs::metadata(gs_file(&sd)).unwrap().modified().unwrap(),
        mtime
    );
}

#[test]
fn sync_keeps_the_group_settings_of_a_patient_without_the_source() {
    let (fx, sd) = sync_fixture("sync_nosource");
    let before = cfg::load(&sd, "1", &label()).unwrap();
    let mut o = options(&fx);
    o.copy_settings_from = Some("missing".to_owned());
    o.sync_settings = true;
    assert_eq!(prepare(&fx, &o).unwrap(), before);
    assert_eq!(cfg::load(&sd, "1", &label()).unwrap(), before);
}

#[test]
fn without_sync_an_existing_group_copy_is_kept() {
    let (fx, sd) = sync_fixture("sync_off");
    let before = cfg::load(&sd, "1", &label()).unwrap();
    let mut o = options(&fx);
    o.copy_settings_from = Some("k3v".to_owned());
    assert_eq!(prepare(&fx, &o).unwrap(), before);
    assert_eq!(cfg::load(&sd, "1", &label()).unwrap(), before);
}
