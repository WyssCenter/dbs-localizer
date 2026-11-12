#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use dbsloc_pipeline::group::clinical::{ClinicalKind, ClinicalVar};
use serde_json::json;

use super::*;

const GUID: &str = "20261023101500";

impl GroupTab {
    fn is_dirty(&self) -> bool {
        matches!(&self.phase, Phase::Open(s) if s.dirty)
    }

    fn open_path(&self) -> Option<&Path> {
        match &self.phase {
            Phase::Open(s) => Some(&s.json_path),
            _ => None,
        }
    }

    fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
}
const MODEL: &str = "Medtronic 3389";

fn side(head: [f64; 3]) -> serde_json::Value {
    let tail = [head[0], head[1], head[2] + 4.5];
    json!({
        "markers": {
            "head": head, "tail": tail,
            "x": [head[0] + 0.64, head[1], head[2]],
            "y": [head[0], head[1] + 0.64, head[2]],
        },
        "coords_mm": [
            [head[0], head[1], head[2]],
            [head[0], head[1], head[2] + 1.5],
            [head[0], head[1], head[2] + 3.0],
            [head[0], head[1], head[2] + 4.5],
        ],
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    template: PathBuf,
    ctx: egui::Context,
}

fn fixture(subjects: &[&str]) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix("dbsloc_group_panel_")
        .tempdir()
        .unwrap();
    let root = dir.path().join("Cohort");
    for id in subjects {
        let rec = root
            .join("derivatives")
            .join("leaddbs")
            .join(format!("sub-{id}"))
            .join("reconstruction");
        std::fs::create_dir_all(&rec).unwrap();
        let reco = json!({
            "version": 2,
            "props": {"1": {"elmodel": MODEL}, "2": {"elmodel": MODEL}},
            "meta": {"side_keys": "1=right,2=left", "template_space": "MNI152NLin2009bAsym"},
            "mni": {"1": side([10.0, -12.0, -8.0]), "2": side([-10.0, -12.0, -8.0])},
        });
        std::fs::write(
            rec.join(format!("sub-{id}_desc-reconstruction.json")),
            reco.to_string(),
        )
        .unwrap();
    }
    let template_dir = dir.path().join("template");
    std::fs::create_dir_all(&template_dir).unwrap();
    std::fs::write(template_dir.join("t1.nii.gz"), b"x").unwrap();
    std::fs::write(
        template_dir.join("template.json"),
        br#"{"space": "MNI152NLin2009bAsym"}"#,
    )
    .unwrap();
    Fixture {
        template: template_dir.join("t1.nii.gz"),
        root,
        _dir: dir,
        ctx: egui::Context::default(),
    }
}

fn env(fx: &Fixture) -> GroupEnv {
    GroupEnv {
        run_active: false,
        dataset_root: Some(fx.root.clone()),
        derivatives_root: Some(fx.root.join("derivatives").join("leaddbs")),
        selected_subject: Some("sub-01".to_owned()),
        ticked_subjects: Vec::new(),
        template: Some(fx.template.to_string_lossy().into_owned()),
        atlases_dir: None,
        recent: Vec::new(),
        connectomes_dir: None,
        last_connectomes: BTreeMap::new(),
    }
}

fn open_session(fx: &Fixture) -> GroupTab {
    let mut tab = GroupTab::default();
    tab.create(fx.root.clone(), GUID.to_owned(), "Test".to_owned(), &fx.ctx);
    wait_until(&mut tab, &fx.ctx, |t| t.is_open());
    tab
}

fn wait_until(
    tab: &mut GroupTab,
    ctx: &egui::Context,
    done: impl Fn(&GroupTab) -> bool,
) -> GroupOutput {
    let mut all = GroupOutput::default();
    let t0 = Instant::now();
    while !done(tab) {
        assert!(t0.elapsed() < Duration::from_secs(60), "timed out");
        all.merge(tab.poll(ctx));
        std::thread::sleep(Duration::from_millis(10));
    }
    all.merge(tab.poll(ctx));
    all
}

fn session(tab: &mut GroupTab) -> &mut Session {
    match &mut tab.phase {
        Phase::Open(s) => s,
        _ => panic!("no open group"),
    }
}

#[test]
fn recent_groups_are_deduplicated_newest_first_and_capped() {
    let mut list = Vec::new();
    for i in 0..12 {
        remember_group(&mut list, &format!("g{i}.json"));
    }
    assert_eq!(list.len(), RECENT_GROUP_LIMIT);
    assert_eq!(list[0], "g11.json");
    remember_group(&mut list, "g8.json");
    assert_eq!(list[0], "g8.json");
    assert_eq!(list.iter().filter(|p| *p == "g8.json").count(), 1);
    remember_group(&mut list, "  ");
    assert_eq!(list.len(), RECENT_GROUP_LIMIT);
}

#[test]
fn recent_groups_survive_a_state_round_trip() {
    let mut state = crate::app::DbslocAppState::default();
    remember_group(&mut state.recent_groups, "D:/a/group.json");
    let text = serde_json::to_string(&state).unwrap();
    let back: crate::app::DbslocAppState = serde_json::from_str(&text).unwrap();
    assert_eq!(back.recent_groups, vec!["D:/a/group.json".to_owned()]);
    let old: crate::app::DbslocAppState = serde_json::from_str(r#"{"bids_dir":"D:/x"}"#).unwrap();
    assert!(old.recent_groups.is_empty());
}

#[test]
fn subject_ids_are_split_cleaned_and_deduplicated() {
    assert_eq!(
        parse_ids("01, sub-02;03\n01  04"),
        vec!["01", "02", "03", "04"]
    );
    assert!(parse_ids(" ,; ").is_empty());
}

#[test]
fn normalisation_codes_follow_the_lead_group_popup() {
    assert_eq!(normalisation_of(1), Normalisation::None);
    assert_eq!(normalisation_of(2), Normalisation::ZScore);
    assert_eq!(normalisation_of(3), Normalisation::VanAlbada);
    assert_eq!(normalisation_of(9), Normalisation::None);
    for n in [
        Normalisation::None,
        Normalisation::ZScore,
        Normalisation::VanAlbada,
    ] {
        assert_eq!(normalisation_of(normalisation_code(n)), n);
    }
}

#[test]
fn the_view_request_follows_the_record() {
    let mut record = GroupRecord::new(GUID, "Cohort");
    record.view.show_passive = false;
    record.view.highlight_active = true;
    record.view.mirror_sides = true;
    record.view.normalisation = 2;
    let env = GroupEnv {
        template: Some("  ".to_owned()),
        ..GroupEnv::default()
    };
    let o = view_options(&record, &env, false);
    assert_eq!(o.colour_by, ColourBy::Group);
    assert_eq!(o.normalisation, Normalisation::ZScore);
    assert!(o.mirror);
    assert!(o.template.is_none());
    let show = o.show.unwrap();
    assert!(show.active_contacts && !show.passive_contacts && show.highlight_active);
    assert!(!show.point_cloud);
    assert!(matches!(o.flip_transform, FlipTransformChoice::Auto));

    record
        .clinical
        .push(ClinicalVar::blank("UPDRS", ClinicalKind::PerPatient, 0, 1));
    record.view.colour_by = vec!["UPDRS".to_owned()];
    assert_eq!(colour_choice(&record), ColourBy::Group);
    record.view.colour_contacts = true;
    assert_eq!(
        colour_choice(&record),
        ColourBy::Clinical("UPDRS".to_owned())
    );
    record.view.colour_by = vec!["gone".to_owned()];
    assert_eq!(colour_choice(&record), ColourBy::Group);
    record.view.colour_by = vec!["UPDRS".to_owned()];
    let o = view_options(&record, &env, true);
    assert!(o.show.unwrap().point_cloud);
    assert!(matches!(o.flip_transform, FlipTransformChoice::PlainFlip));
}

#[test]
fn the_calculate_request_follows_the_record_and_the_checkboxes() {
    let mut record = GroupRecord::new(GUID, "Cohort");
    let run = RunOptions::new(PathBuf::new(), "");
    let o = calc_options(run.clone(), &record, false, " ");
    assert!(!o.mirror && o.copy_settings_from.is_none() && !o.sync_settings);
    assert!(matches!(o.flip_transform, FlipTransformChoice::Auto));
    record.view.mirror_sides = true;
    let o = calc_options(run, &record, true, "20260101");
    assert!(o.mirror);
    assert_eq!(o.copy_settings_from.as_deref(), Some("20260101"));
    assert!(o.sync_settings);
    assert!(matches!(o.flip_transform, FlipTransformChoice::PlainFlip));
}

#[test]
fn opening_a_missing_file_ends_idle_with_a_notice() {
    let fx = fixture(&[]);
    let mut tab = GroupTab::default();
    tab.open(fx.root.join("nope.json"), &fx.ctx);
    assert!(tab.is_opening());
    let out = wait_until(&mut tab, &fx.ctx, |t| !t.is_opening());
    assert!(!tab.is_open());
    assert!(tab.notice().is_some());
    assert_eq!(out.errors.len(), 1);
}

#[test]
fn creating_a_group_opens_it_and_remembers_the_path() {
    let fx = fixture(&["01"]);
    let mut tab = GroupTab::default();
    tab.create(
        fx.root.clone(),
        GUID.to_owned(),
        "Title".to_owned(),
        &fx.ctx,
    );
    let out = wait_until(&mut tab, &fx.ctx, |t| t.is_open());
    assert!(tab.is_settled());
    let path = out.remember.expect("remembered");
    assert!(
        path.ends_with(&format!("dataset-Cohort_analysis-{GUID}.json")),
        "{path}"
    );
    assert!(out.focus);
    assert_eq!(tab.open_path().unwrap(), Path::new(&path));
    assert_eq!(session(&mut tab).record.title, "Title");
    tab.close();
    assert!(!tab.is_open());
}

#[test]
fn adding_patients_registers_the_dataset_and_marks_the_group_changed() {
    let fx = fixture(&["01", "02"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(
        session(&mut tab),
        &e,
        &["01".to_owned(), "02".to_owned()],
        &mut out,
    );
    let s = session(&mut tab);
    assert_eq!(s.record.patients.len(), 2);
    assert_eq!(s.record.datasets.len(), 1);
    assert!(s.dirty);
    assert_eq!(s.has_reco, vec![true, true]);
    add_patients(s, &e, &["01".to_owned()], &mut out);
    assert_eq!(out.errors.len(), 1);
    assert_eq!(s.record.patients.len(), 2);
    assert!(s.save(&mut out));
    assert!(!s.dirty);
    let reopened = open_group(&s.json_path).unwrap();
    assert_eq!(reopened.record.patients.len(), 2);
}

#[test]
fn a_clinical_table_is_imported_and_a_second_import_needs_replace() {
    let fx = fixture(&["01", "02"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(
        session(&mut tab),
        &e,
        &["01".to_owned(), "02".to_owned()],
        &mut out,
    );
    let table = fx.root.join("clinical.tsv");
    std::fs::write(
        &table,
        "participant_id\tUPDRS\n01\t10\nsub-02\t30\nsub-99\t1\n",
    )
    .unwrap();
    let s = session(&mut tab);
    import_clinical(s, &table, &mut out);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(s.record.clinical.len(), 1);
    assert_eq!(s.record.clinical[0].values.len(), 2);
    assert!(out.log.iter().any(|l| l.contains("sub-99")));
    import_clinical(s, &table, &mut out);
    assert_eq!(out.errors.len(), 1);
    s.replace_clinical = true;
    import_clinical(s, &table, &mut out);
    assert_eq!(out.errors.len(), 1);
    import_clinical(s, &fx.root.join("missing.tsv"), &mut out);
    assert_eq!(out.errors.len(), 2);
}

#[test]
fn show_in_the_viewer_writes_the_manifest_and_asks_the_app_to_load_it() {
    let fx = fixture(&["01", "02"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(
        session(&mut tab),
        &e,
        &["01".to_owned(), "02".to_owned()],
        &mut out,
    );
    session(&mut tab).selected = Some(1);
    tab.start_view(&e, &fx.ctx);
    assert!(tab.is_busy());
    assert!(!tab.is_settled());
    let out = wait_until(&mut tab, &fx.ctx, |t| t.is_settled());
    let manifest = out.show_scene.expect("manifest path");
    assert!(
        manifest.ends_with("viz/scene_manifest.json")
            || manifest.ends_with("viz\\scene_manifest.json")
    );
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    assert_eq!(value["manifest_version"], 2);
    assert_eq!(value["group"]["subjects"].as_array().unwrap().len(), 2);
    let look = out.look.expect("look");
    assert!(look.show_active && look.show_passive && !look.highlight_active);
    assert_eq!(look.selected_patient.as_deref(), Some("sub-02"));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
}

#[test]
fn a_view_without_any_reconstruction_reports_an_error_and_settles() {
    let fx = fixture(&[]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(session(&mut tab), &e, &["77".to_owned()], &mut out);
    tab.start_view(&e, &fx.ctx);
    let out = wait_until(&mut tab, &fx.ctx, |t| t.is_settled());
    assert!(out.show_scene.is_none());
    assert_eq!(out.errors.len(), 1);
    assert_eq!(out.errors[0].0, "Group 3D view");
}

#[test]
fn calculate_without_settings_fails_per_patient() {
    let fx = fixture(&["01"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(session(&mut tab), &e, &["01".to_owned()], &mut out);
    let mut run = RunOptions::new(fx.root.clone(), "");
    run.use_index = false;
    run.headless = true;
    run.checkreg = false;
    run.no_scene3d = true;
    let start = tab.start_calc(run, &fx.ctx);
    assert!(start.errors.is_empty(), "{:?}", start.errors);
    assert!(tab.is_busy());
    let out = wait_until(&mut tab, &fx.ctx, |t| t.is_settled());
    assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
    assert!(out.errors[0].1.contains("sub-01"));
    assert!(out.log.iter().any(|l| l.contains("sub-01")));
    assert!(!tab.is_dirty());
}

#[test]
fn export_writes_the_clinical_table_and_reports_the_files() {
    let fx = fixture(&["01"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(session(&mut tab), &e, &["01".to_owned()], &mut out);
    let mut v = ClinicalVar::blank("UPDRS", ClinicalKind::PerPatient, 1, 1);
    v.values = vec![vec![Some(12.0)]];
    session(&mut tab).record.clinical.push(v);
    tab.start_export(&fx.ctx);
    let out = wait_until(&mut tab, &fx.ctx, |t| t.is_settled());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(
        out.log.iter().any(|l| l.contains("desc-clinical.tsv")),
        "{:?}",
        out.log
    );
}

#[test]
fn the_look_follows_the_selection_and_the_display_flags() {
    let fx = fixture(&["01", "02"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_patients(
        session(&mut tab),
        &e,
        &["01".to_owned(), "02".to_owned()],
        &mut out,
    );
    let s = session(&mut tab);
    assert_eq!(s.look().selected_patient, None);
    s.selected = Some(0);
    s.selected_group = Some(1);
    s.record.view.highlight_active = true;
    let look = s.look();
    assert_eq!(look.selected_patient.as_deref(), Some("sub-01"));
    assert_eq!(look.selected_group, Some(1));
    assert!(look.highlight_active);
}

#[test]
fn edit_targets_the_source_label_or_the_group_label() {
    let f = edit_focus("sub-01", None, "gs_abc");
    assert_eq!((f.label.as_str(), f.group_label), ("gs_abc", true));
    let f = edit_focus("sub-01", Some("  "), "gs_abc");
    assert!(f.group_label);
    let f = edit_focus("sub-01", Some(" clinical "), "gs_abc");
    assert_eq!((f.label.as_str(), f.group_label), ("clinical", false));
    assert_eq!(f.subject, "sub-01");
}

#[test]
fn edit_is_blocked_with_a_reason() {
    assert_eq!(edit_block_reason(true, true, false), None);
    assert!(edit_block_reason(false, true, false).is_some_and(|r| r.contains("reconstruction")));
    assert!(edit_block_reason(true, false, false).is_some_and(|r| r.contains("input directory")));
    assert!(edit_block_reason(true, true, true).is_some_and(|r| r.contains("running")));
}

#[test]
fn outputs_merge_the_latest_requests() {
    let mut a = GroupOutput::default();
    a.log.push("a".into());
    a.show_scene = Some(PathBuf::from("one"));
    let mut b = GroupOutput::default();
    b.log.push("b".into());
    b.show_scene = Some(PathBuf::from("two"));
    b.calc = true;
    a.merge(b);
    assert_eq!(a.log, vec!["a", "b"]);
    assert_eq!(a.show_scene, Some(PathBuf::from("two")));
    assert!(a.calc);
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_add_button_prefers_the_ticked_subjects() {
    let c = add_choice(&names(&["sub-01", "sub-02", "sub-01"]), Some("sub-09"));
    assert_eq!(c.ids, names(&["01", "02"]));
    assert_eq!(c.label, "Add ticked subjects (2)");
    assert!(c.ticked);
    let c = add_choice(&[], Some("sub-09"));
    assert_eq!(c.ids, names(&["09"]));
    assert_eq!(c.label, "Add sub-09");
    assert!(!c.ticked);
    let c = add_choice(&[], None);
    assert!(c.ids.is_empty());
}

#[test]
fn adding_chosen_patients_skips_the_ones_already_present() {
    let fx = fixture(&["01", "02"]);
    let mut tab = open_session(&fx);
    let e = env(&fx);
    let mut out = GroupOutput::default();
    add_chosen(session(&mut tab), &e, &names(&["01"]), &mut out);
    add_chosen(session(&mut tab), &e, &names(&["01", "02"]), &mut out);
    let s = session(&mut tab);
    assert_eq!(s.record.patients.len(), 2);
    assert!(out.errors.is_empty());
    assert_eq!(
        s.status.as_deref(),
        Some("Added 1 patient · 1 already in the group")
    );
    assert!(out.log.iter().any(|l| l.contains("skipped 1")));
}

fn write_file(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"{}").unwrap();
}

#[test]
fn the_scan_tells_each_patient_what_is_missing() {
    let fx = fixture(&["01", "02", "03", "04"]);
    let base = fx.root.join("derivatives").join("leaddbs");
    let label = format!("gs_{GUID}");
    let native = |id: &str, l: &str| {
        base.join(format!("sub-{id}"))
            .join("stimulations")
            .join("native")
            .join(l)
            .join(format!("sub-{id}_desc-stimulation.json"))
    };
    write_file(&native("01", "clinical"));
    write_file(&native("02", &label));
    write_file(&native("04", &label));
    write_file(
        &base
            .join("sub-04")
            .join("stimulations")
            .join("MNI152NLin2009bAsym")
            .join(&label)
            .join("sub-04_desc-vta.json"),
    );
    std::fs::remove_file(reco_file(&base.join("sub-03"), "sub-03")).unwrap();
    let patients: Vec<(String, Option<PathBuf>)> = ["01", "02", "03", "04"]
        .iter()
        .map(|id| (format!("sub-{id}"), Some(base.join(format!("sub-{id}")))))
        .collect();
    let key = ScanKey {
        patients: patients.clone(),
        label: label.clone(),
        source: None,
    };
    let r = scan_patients(&key);
    assert_eq!(r.statuses["sub-01"], PatientStatus::NoStimulation);
    assert_eq!(r.statuses["sub-02"], PatientStatus::Ready(None));
    assert_eq!(r.statuses["sub-03"], PatientStatus::NoReconstruction);
    assert_eq!(r.statuses["sub-04"], PatientStatus::Computed);
    assert_eq!(r.labels, vec![("clinical".to_owned(), 1)]);
    assert_eq!(
        default_source(&r.labels, &r.statuses).as_deref(),
        None,
        "a patient already has group settings"
    );
    let r = scan_patients(&ScanKey {
        source: Some("clinical".to_owned()),
        ..key
    });
    assert_eq!(
        r.statuses["sub-01"],
        PatientStatus::Ready(Some("clinical".to_owned()))
    );
    assert_eq!(r.statuses["sub-02"], PatientStatus::Ready(None));
}

#[test]
fn a_single_label_is_preselected_when_nobody_has_group_settings() {
    let labels = vec![("clinical".to_owned(), 2)];
    let mut st = HashMap::new();
    st.insert("sub-01".to_owned(), PatientStatus::NoStimulation);
    st.insert("sub-02".to_owned(), PatientStatus::NoReconstruction);
    assert_eq!(default_source(&labels, &st).as_deref(), Some("clinical"));
    st.insert("sub-03".to_owned(), PatientStatus::Ready(None));
    assert_eq!(default_source(&labels, &st), None);
    let two = vec![("a".to_owned(), 1), ("b".to_owned(), 1)];
    st.clear();
    st.insert("sub-01".to_owned(), PatientStatus::NoStimulation);
    assert_eq!(default_source(&two, &st), None);
}

#[test]
fn the_count_line_lists_only_non_zero_parts() {
    let st = [
        PatientStatus::Ready(None),
        PatientStatus::Ready(None),
        PatientStatus::NoStimulation,
    ];
    assert_eq!(
        status_line(3, st.iter()),
        "3 patients · 2 ready · 1 without stimulation"
    );
    assert_eq!(
        status_line(1, [PatientStatus::Computed].iter()),
        "1 patient · 1 computed"
    );
}

#[test]
fn the_compute_summary_names_the_steps() {
    assert_eq!(
        compute_summary(8, Some("clinical"), "DISTAL Minimal", false),
        "For 8 patients: VTA from \"clinical\" → atlas overlap with DISTAL Minimal → contact distances"
    );
    assert_eq!(
        compute_summary(1, None, "X", true),
        "For 1 patient: VTA from group settings → atlas overlap with X → contact distances → mirrored volumes"
    );
    assert_eq!(
        compute_summary(0, None, "X", false),
        "Nothing to compute: no patient is ready"
    );
}

#[test]
fn source_and_atlas_labels_read_plainly() {
    let labels = vec![("clinical".to_owned(), 8)];
    assert_eq!(source_label("clinical", &labels, 10), "clinical (8 of 10)");
    assert_eq!(source_label("other", &labels, 10), "other");
    let list = AtlasList {
        entries: vec![("leaddbs:D".to_owned(), "DISTAL".to_owned())],
        default_name: Some("DISTAL".to_owned()),
    };
    assert_eq!(atlas_text(Some(&list), None), "Default (DISTAL)");
    assert_eq!(atlas_text(Some(&list), Some("leaddbs:D")), "DISTAL");
    assert_eq!(atlas_text(Some(&list), Some("gone")), "gone");
}

#[test]
fn the_export_folder_is_the_deepest_folder_all_files_share() {
    let files = vec![
        PathBuf::from("g/exports/a.tsv"),
        PathBuf::from("g/exports/b.csv"),
    ];
    assert_eq!(common_parent(&files), Some(PathBuf::from("g/exports")));
    let mixed = vec![
        PathBuf::from("g/exports/a.tsv"),
        PathBuf::from("g/clinical/c.tsv"),
    ];
    assert_eq!(common_parent(&mixed), Some(PathBuf::from("g")));
    assert_eq!(common_parent(&[]), None);
}
