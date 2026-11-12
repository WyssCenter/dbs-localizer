// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::{Path, PathBuf};

use dbsloc_pipeline::vat::settings::{
    save, CaseSpec, Contact, Mode, ModelKind, Polarity, Side, Stimulation,
};
use serde_json::{json, Value};

use common::{run_args, s, write};

const GUID: &str = "20261023101500";
const MODEL: &str = "Medtronic 3389";
const SPACE: &str = "MNI152NLin2009bAsym";

fn markers(head: [f64; 3]) -> Value {
    let tail = [head[0], head[1], head[2] + 6.0];
    json!({
        "head": head,
        "tail": tail,
        "x": [head[0] + 0.635, head[1], head[2]],
        "y": [head[0], head[1] + 0.635, head[2]],
    })
}

fn subject_dir(root: &Path, id: &str) -> PathBuf {
    root.join("derivatives")
        .join("leaddbs")
        .join(format!("sub-{id}"))
}

fn make_subject(root: &Path, id: &str, with_reco: bool) {
    write(
        &root
            .join(format!("rawdata/sub-{id}/ses-preop/anat"))
            .join(format!("sub-{id}_ses-preop_T1w.nii.gz")),
        b"x",
    );
    let dir = subject_dir(root, id);
    if with_reco {
        let reco = json!({
            "version": 2,
            "subj_id": id,
            "props": {"1": {"elmodel": MODEL}, "2": {"elmodel": MODEL}},
            "meta": {"side_keys": "1=right,2=left"},
            "native": {
                "1": {"markers": markers([10.0, -12.0, -8.0])},
                "2": {"markers": markers([-10.0, -12.0, -8.0])},
            },
        });
        write(
            &dir.join(format!("reconstruction/sub-{id}_desc-reconstruction.json")),
            serde_json::to_string_pretty(&reco)
                .unwrap_or_else(|e| panic!("{e}"))
                .as_bytes(),
        );
    } else {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }
    let mut stim = Stimulation::blank(&format!("gs_{GUID}"), id, ModelKind::Kuncel, [4, 4]);
    let src = &mut stim.sides.get_mut(Side::Right).sources[0];
    src.mode = Mode::Voltage;
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
    save(&dir, &stim).unwrap_or_else(|e| panic!("{e}"));
}

fn setup(tmp: &Path, order: [&str; 2]) -> (PathBuf, PathBuf) {
    let root = tmp.join("Cohort");
    make_subject(&root, "01", true);
    make_subject(&root, "02", false);
    let out = run_args(&["group", "create", &s(&root), "--guid", GUID]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let folder = root.join("derivatives").join("leadgroup").join(GUID);
    let out = run_args(&["group", "add", &s(&folder), &s(&root), order[0], order[1]]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    (root, folder)
}

#[test]
fn calc_runs_the_batch_reports_the_failed_patient_and_keeps_on_rerun() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (root, folder) = setup(tmp.path(), ["01", "02"]);
    let out = run_args(&[
        "group",
        "calc",
        &s(&folder),
        "--no-contact-distances",
        "--json",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["label"], format!("gs_{GUID}"));
    assert_eq!(json["subjects"][0]["subject"], "sub-01");
    assert_eq!(json["subjects"][0]["status"], "computed", "{json}");
    assert_eq!(json["subjects"][1]["status"], "not_run");
    assert_eq!(json["subjects"][1]["message"], "no reconstruction");
    assert_eq!(json["totals"]["failed"], 1);
    assert!(subject_dir(&root, "01")
        .join(format!(
            "stimulations/native/gs_{GUID}/sub-01_sim-binary_model-kuncel_hemi-R.nii"
        ))
        .is_file());
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(format!("dataset-Cohort_analysis-{GUID}.json")))
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(record["analyses"][0]["kind"], "atlasstats");
    assert_eq!(record["stimulation"]["model"], "kuncel");
    assert!(folder.join("atlasstats/calc/analysis.json").is_file());

    let again = run_args(&[
        "group",
        "calc",
        &s(&folder),
        "--subjects",
        "01",
        "--no-contact-distances",
        "--json",
    ]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    assert_eq!(again.json()["subjects"][0]["status"], "kept");
}

#[test]
fn stop_on_first_failure_reports_the_rest_as_cancelled() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (_root, folder) = setup(tmp.path(), ["02", "01"]);
    let out = run_args(&[
        "group",
        "calc",
        &s(&folder),
        "--stop-on-first-failure",
        "--no-contact-distances",
        "--json",
    ]);
    assert_eq!(out.code, 1, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["subjects"][0]["status"], "not_run");
    assert_eq!(json["subjects"][1]["status"], "cancelled");
}

#[test]
fn an_unknown_patient_is_an_error() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (_root, folder) = setup(tmp.path(), ["01", "02"]);
    let out = run_args(&["group", "calc", &s(&folder), "--subjects", "99"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("not in the group"), "{}", out.stderr);
}

#[test]
fn stats_writes_the_tables_with_the_spec_columns() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (root, folder) = setup(tmp.path(), ["01", "02"]);
    let side = |volume: f64, overlap: f64| {
        json!({
            "amplitudes": [3.0, 0.0, 0.0, 0.0],
            "volume_mm3": volume,
            "atlas_volume_mm3": [20.0],
            "intersection_mm3": [overlap],
            "n_intersection": [overlap / volume],
            "n_within_intersection": [overlap / 20.0],
            "efield": {"volume": 0.0, "intersection": [0.0], "n_intersection": [0.0]},
        })
    };
    let stats = json!({
        "schema": "dbsloc-vtastats",
        "label": format!("gs_{GUID}"),
        "atlas": {"structures": [{"name": "STN.nii.gz", "type": 3, "skipped": false}]},
        "sides": {"right": side(10.0, 4.0), "left": side(8.0, 2.0)},
    });
    write(
        &subject_dir(&root, "01").join(format!(
            "stimulations/{SPACE}/gs_{GUID}/sub-01_desc-vtastats.json"
        )),
        stats.to_string().as_bytes(),
    );
    let export = tmp.path().join("out");
    let out = run_args(&[
        "group",
        "stats",
        &s(&folder),
        "--no-contact-distances",
        "--export",
        &s(&export),
        "--csv",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["export"]["vta_rows"], 2);
    assert_eq!(json["export"]["patients_with_vta"], 1);
    assert_eq!(json["export"]["missing_vta"], json!(["sub-02"]));
    let stem = format!("dataset-Cohort_analysis-{GUID}");
    let tsv = std::fs::read_to_string(export.join(format!("{stem}_desc-atlasoverlap_stats.tsv")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        tsv.starts_with("subject\tgroup\tgroup_label\tside\tsource\tstructure\tvta_volume_mm3\t")
    );
    assert_eq!(tsv.lines().count(), 3);
    assert!(tsv
        .lines()
        .nth(1)
        .is_some_and(|l| l.starts_with("sub-01\t1\t")));
    assert!(export
        .join(format!("{stem}_desc-atlasoverlapsummary_stats.csv"))
        .is_file());
    assert!(export
        .join(format!("{stem}_desc-contactdistance_stats.tsv"))
        .is_file());
}

#[test]
fn plain_flip_and_a_flip_transform_exclude_each_other() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (_root, folder) = setup(tmp.path(), ["01", "02"]);
    let out = run_args(&[
        "group",
        "calc",
        &s(&folder),
        "--mirror",
        "--plain-flip",
        "--flip-transform",
        "flip.h5",
    ]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("cannot be used with"), "{}", out.stderr);
    let out = run_args(&[
        "group",
        "calc",
        &s(&folder),
        "--subjects",
        "01",
        "--no-contact-distances",
        "--mirror",
        "--plain-flip",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["subjects"][0]["status"], "computed");
}

#[test]
fn fliplr_show_reports_the_cache_of_a_template_space() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let data = tmp.path().join("data");
    let t1 = tmp.path().join("templates/space/Phantom/res-1mm/t1.nii.gz");
    write(&t1, b"template");
    let show = |extra: &[&str]| {
        let mut c = common::dbsloc();
        c.env("DBSLOC_DATA_DIR", &data)
            .args(["group", "fliplr", "show", "--template", &s(&t1)])
            .args(extra);
        common::run(&mut c)
    };
    let out = show(&["--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["space"], "Phantom");
    assert_eq!(json["found"], false);
    let cache = data
        .join("templates")
        .join("space")
        .join("Phantom")
        .join("fliplr");
    assert_eq!(json["cache_dir"], s(&cache));

    write(
        &cache.join("from-Phantom_to-PhantomFlipped_mode-image_xfm.h5"),
        b"f",
    );
    write(
        &cache.join("from-PhantomFlipped_to-Phantom_mode-image_xfm.h5"),
        b"i",
    );
    let json = show(&["--json"]).json();
    assert_eq!(json["found"], true);
    assert_eq!(json["stale_cache"], false);
    let text = show(&[]);
    assert!(text.stdout.contains("forward"), "{}", text.stdout);

    let missing = common::run(common::dbsloc().args([
        "group",
        "fliplr",
        "show",
        "--template",
        &s(&tmp.path().join("none")),
    ]));
    assert_eq!(missing.code, 1);
}

#[test]
fn sync_settings_needs_a_settings_source() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (_root, folder) = setup(tmp.path(), ["01", "02"]);
    let out = run_args(&["group", "calc", &s(&folder), "--sync-settings"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("--copy-settings-from"),
        "{}",
        out.stderr
    );
}
