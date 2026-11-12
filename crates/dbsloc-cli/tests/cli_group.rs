// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

mod common;

use std::path::Path;

use common::{run_args, s, write};

const GUID: &str = "20261023101500";

fn dataset(root: &Path) {
    for sub in ["sub-01", "sub-02"] {
        std::fs::create_dir_all(root.join("derivatives").join("leaddbs").join(sub))
            .unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn create_add_set_group_clinical_info_and_export() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = tmp.path().join("MyCohort");
    dataset(&root);
    let folder = root.join("derivatives").join("leadgroup").join(GUID);
    let group_file = folder.join(format!("dataset-MyCohort_analysis-{GUID}.json"));
    let twin = folder.join(format!("dataset-MyCohort_analysis-{GUID}.mat"));

    let out = run_args(&["group", "create", &s(&root), "--guid", GUID, "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(group_file.is_file());
    assert!(twin.is_file());
    assert_eq!(out.json()["guid"], GUID);

    let out = run_args(&[
        "group",
        "add",
        &s(&folder),
        &s(&root),
        "01",
        "sub-02",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["added"], serde_json::json!(["sub-01", "sub-02"]));

    let out = run_args(&["group", "add", &s(&folder), &s(&root), "01"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("already in the group"),
        "{}",
        out.stderr
    );

    let out = run_args(&[
        "group",
        "set-group",
        &s(&group_file),
        "2",
        "02",
        "--label",
        "Treated",
        "--color",
        "#FF8000",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    let table = tmp.path().join("clinical.tsv");
    write(
        &table,
        b"participant_id\tUPDRS\tscore_R\tscore_L\n01\t12.5\t1\t2\nsub-02\tNA\t3\t\nsub-99\t1\t1\t1\n",
    );
    let out = run_args(&[
        "group",
        "clinical",
        "import",
        &s(&folder),
        &s(&table),
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.json()["unknown_subjects"],
        serde_json::json!(["sub-99"])
    );
    assert!(out.stderr.contains("sub-99"));

    let out = run_args(&["group", "clinical", "import", &s(&folder), &s(&table)]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("already exist"), "{}", out.stderr);

    let out = run_args(&["group", "info", &s(&folder), "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let info = out.json();
    assert_eq!(info["patients"].as_array().map(Vec::len), Some(2));
    assert_eq!(info["patients"][1]["group"], 2);
    assert_eq!(info["patients"][1]["group_label"], "Treated");
    assert_eq!(info["groups"].as_array().map(Vec::len), Some(2));
    let clinical = info["clinical"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = clinical.iter().filter_map(|v| v["name"].as_str()).collect();
    assert_eq!(names, ["UPDRS", "score"]);
    assert_eq!(info["clinical"][1]["kind"], "per_hemisphere");
    assert_eq!(info["lead_group_file"]["in_sync"], true);

    std::fs::remove_file(&twin).unwrap_or_else(|e| panic!("{e}"));
    let out = run_args(&["group", "export", &s(&folder), "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(twin.is_file());
    let tsv = folder.join("exports").join(format!(
        "dataset-MyCohort_analysis-{GUID}_desc-clinical.tsv"
    ));
    let text = std::fs::read_to_string(&tsv).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        text,
        "participant_id\tgroup\tgroup_label\tUPDRS\tscore_R\tscore_L\nsub-01\t1\tGroup 1\t12.5\t1\t2\nsub-02\t2\tTreated\t\t3\t\n"
    );
    assert!(tsv.with_extension("json").is_file());

    let bytes = std::fs::read(&twin).unwrap_or_else(|e| panic!("{e}"));
    assert!(bytes.starts_with(b"MATLAB 5.0 MAT-file"));

    let out = run_args(&["group", "remove", &s(&folder), "01", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_args(&["group", "info", &s(&folder), "--json"]);
    assert_eq!(out.json()["patients"].as_array().map(Vec::len), Some(1));
}

#[test]
fn view_writes_a_version_two_manifest_and_validates_its_options() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = tmp.path().join("MyCohort");
    dataset(&root);
    let reco = serde_json::json!({
        "version": 2,
        "subj_id": "01",
        "props": {"1": {"elmodel": "Medtronic 3389"}},
        "meta": {"side_keys": "1=right,2=left", "template_space": "MNI152NLin2009bAsym"},
        "mni": {"1": {
            "markers": {"head": [10.0, -12.0, -8.0], "tail": [10.0, -12.0, 1.0]},
            "coords_mm": [[10.0, -12.0, -8.0], [10.0, -12.0, -6.5]],
        }},
    });
    let reco_dir = root
        .join("derivatives")
        .join("leaddbs")
        .join("sub-01")
        .join("reconstruction");
    write(
        &reco_dir.join("sub-01_desc-reconstruction.json"),
        reco.to_string().as_bytes(),
    );
    let template_dir = tmp.path().join("template");
    write(&template_dir.join("t1.nii.gz"), b"x");
    write(
        &template_dir.join("template.json"),
        br#"{"space": "MNI152NLin2009bAsym"}"#,
    );
    let template = template_dir.join("t1.nii.gz");
    let folder = root.join("derivatives").join("leadgroup").join(GUID);
    let out = run_args(&["group", "create", &s(&root), "--guid", GUID]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = run_args(&["group", "add", &s(&folder), &s(&root), "01"]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    let out = run_args(&[
        "group",
        "view",
        &s(&folder),
        "--template",
        &s(&template),
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let manifest = folder.join("viz").join("scene_manifest.json");
    assert_eq!(out.json()["manifest"], serde_json::json!(s(&manifest)));
    assert_eq!(out.json()["leads"], 1);
    let text = std::fs::read_to_string(&manifest).unwrap_or_else(|e| panic!("{e}"));
    let written: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(written["manifest_version"], 2);
    assert_eq!(written["group"]["subjects"][0]["subject"], "sub-01");

    let out = run_args(&[
        "group",
        "view",
        &s(&folder),
        "--template",
        &s(&template),
        "--colour-by",
        "missing",
    ]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing"), "{}", out.stderr);

    let out = run_args(&[
        "group",
        "view",
        &s(&folder),
        "--template",
        &s(&template),
        "--normalisation",
        "bogus",
    ]);
    assert_eq!(out.code, 2);
}

fn vtastats(overlap: f64) -> String {
    let side = |overlap: f64| {
        serde_json::json!({
            "amplitudes": [3.0, 0.0, 0.0, 0.0],
            "volume_mm3": 10.0,
            "atlas_volume_mm3": [20.0],
            "intersection_mm3": [overlap],
            "n_intersection": [overlap / 10.0],
            "n_within_intersection": [overlap / 20.0],
            "efield": {"volume": 0.0, "intersection": [0.0], "n_intersection": [0.0]},
        })
    };
    serde_json::json!({
        "schema": "dbsloc-vtastats",
        "label": format!("gs_{GUID}"),
        "atlas": {"structures": [{"name": "STN.nii.gz", "type": 3, "skipped": false}]},
        "sides": {"right": side(overlap), "left": side(overlap / 2.0)},
    })
    .to_string()
}

#[test]
fn correlate_writes_correlation_and_group_tables() {
    let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let root = tmp.path().join("Cohort");
    let ids = ["01", "02", "03", "04"];
    for id in ids {
        std::fs::create_dir_all(root.join("derivatives/leaddbs").join(format!("sub-{id}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let folder = root.join("derivatives").join("leadgroup").join(GUID);
    let out = run_args(&["group", "create", &s(&root), "--guid", GUID]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let mut add = vec!["group", "add"];
    let folder_text = s(&folder);
    let root_text = s(&root);
    add.push(&folder_text);
    add.push(&root_text);
    add.extend(ids);
    let out = run_args(&add);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for (id, overlap) in ids.iter().zip([1.0, 2.0, 5.0, 4.0]) {
        write(
            &root
                .join("derivatives/leaddbs")
                .join(format!("sub-{id}"))
                .join(format!(
                    "stimulations/MNI152NLin2009bAsym/gs_{GUID}/sub-{id}_desc-vtastats.json"
                )),
            vtastats(overlap).as_bytes(),
        );
    }
    let table = tmp.path().join("clinical.tsv");
    write(
        &table,
        b"participant_id	score	responder
01	1	0
02	2	0
03	4	1
04	3	1
",
    );
    let out = run_args(&["group", "clinical", "import", &folder_text, &s(&table)]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    let export = tmp.path().join("out");
    let out = run_args(&[
        "group",
        "correlate",
        &folder_text,
        "--variable",
        "score",
        "--sides",
        "right",
        "--seed",
        "7",
        "--export",
        &s(&export),
        "--csv",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["variable"], "score");
    assert_eq!(json["seed"], 7);
    assert_eq!(json["correlations"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["correlations"][0]["n"], 4);
    assert_eq!(json["correlations"][0]["side"], "right");
    assert_eq!(json["group_tests"].as_array().map(Vec::len), Some(0));
    let stem = format!("dataset-Cohort_analysis-{GUID}");
    let tsv =
        std::fs::read_to_string(export.join(format!("{stem}_desc-clinicalcorrelation_stats.tsv")))
            .unwrap_or_else(|e| panic!("{e}"));
    assert!(tsv.starts_with(
        "structure	measure	side	variable	n	pearson_r	pearson_p_perm	spearman_r	spearman_p_perm
"
    ));
    assert!(tsv.contains("STN	overlap_mm3	right	score	4	"));
    assert!(export
        .join(format!("{stem}_desc-clinicalcorrelation_stats.csv"))
        .is_file());
    assert!(!export
        .join(format!("{stem}_desc-clinicalgroupcomparison_stats.tsv"))
        .exists());
    assert!(folder
        .join("atlasstats/clinical-score-overlap-mm3/analysis.json")
        .is_file());

    let out = run_args(&[
        "group",
        "correlate",
        &folder_text,
        "--variable",
        "responder",
        "--measure",
        "frac_of_vta",
        "--permutations",
        "100",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let json = out.json();
    assert_eq!(json["correlations"].as_array().map(Vec::len), Some(3));
    assert_eq!(json["group_tests"].as_array().map(Vec::len), Some(3));
    assert_eq!(json["group_tests"][0]["boxplot"][0]["n"], 2);
    assert!(folder
        .join("exports")
        .join(format!("{stem}_desc-clinicalgroupcomparison_stats.tsv"))
        .is_file());

    let out = run_args(&["group", "correlate", &folder_text, "--variable", "nope"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("no clinical variable"),
        "{}",
        out.stderr
    );
    let out = run_args(&[
        "group",
        "correlate",
        &folder_text,
        "--variable",
        "score",
        "--measure",
        "volume",
    ]);
    assert_ne!(out.code, 0);
}
