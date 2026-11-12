// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests of the stimulation, Lead Group and connectome endpoints of the
//! `dbsloc-server` router (`docs/SERVER_CLIENT_PLAN.md` §12). GPU-free: jobs use the fake runner
//! scripts, or the built `dbsloc_pipeline_runner` when it exists next to the test binaries.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::vat::settings::{ModelKind, Polarity, Stimulation};
use dbsloc_server::{build_router, AppState, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn scratch_root() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let anat = d.path().join("datasets/demo/rawdata/sub-01/ses-preop/anat");
    std::fs::create_dir_all(&anat).unwrap();
    std::fs::write(anat.join("sub-01_ses-preop_T1w.nii.gz"), b"hello").unwrap();
    let tpl = d.path().join("templates");
    std::fs::create_dir_all(&tpl).unwrap();
    std::fs::write(tpl.join("t1.nii.gz"), b"x").unwrap();
    d
}

fn fake_runner() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    dir.join(if cfg!(windows) {
        "fake_runner.cmd"
    } else {
        "fake_runner.sh"
    })
}

fn built_runner() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug_dir = exe.parent()?.parent()?;
    let name = if cfg!(windows) {
        "dbsloc_pipeline_runner.exe"
    } else {
        "dbsloc_pipeline_runner"
    };
    let p = debug_dir.join(name);
    p.is_file().then_some(p)
}

fn config(root: &Path, runner: PathBuf) -> Config {
    let mut c = Config::default_for_root(root);
    c.templates_dir = Some(root.join("templates"));
    c.pipeline_runner_bin = Some(runner);
    c
}

fn idle_app(root: &Path) -> (AppState, Router) {
    let state = AppState::new_idle(config(root, fake_runner())).unwrap();
    let router = build_router(state.clone());
    (state, router)
}

fn live_app(root: &Path, runner: PathBuf) -> (AppState, Router) {
    let state = AppState::new(config(root, runner)).unwrap();
    let router = build_router(state.clone());
    (state, router)
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn with_json(method: &str, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn post_json(uri: &str, body: &Value) -> Request<Body> {
    with_json("POST", uri, body)
}

fn put_json(uri: &str, body: &Value) -> Request<Body> {
    with_json("PUT", uri, body)
}

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not JSON: {e}: {}", String::from_utf8_lossy(bytes)))
}

fn multipart(uri: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Request<Body> {
    let boundary = "----dbslocTestBoundary";
    let mut body = Vec::new();
    for (name, file, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file {
            Some(f) => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
                    .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn poll_until<F: Fn(&Value) -> bool>(app: &Router, uri: &str, done: F) -> Value {
    for _ in 0..400 {
        let (_, bytes) = call(app, get(uri)).await;
        let v = value(&bytes);
        if done(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out polling {uri}");
}

fn subj_dir(root: &Path) -> PathBuf {
    root.join("datasets/demo/derivatives/leaddbs/sub-01")
}

const STIM: &str = "/api/v1/datasets/demo/subjects/01/stimulations";

fn active_stimulation(label: &str) -> Stimulation {
    let mut s = Stimulation::blank(label, "01", ModelKind::Efield, [4, 4]);
    let src = &mut s.sides.right.sources[0];
    src.amplitude = 2.0;
    src.contacts[1].polarity = Polarity::Cathode;
    src.contacts[1].percent = 100.0;
    s
}

#[tokio::test]
async fn stimulation_settings_round_trip_with_validation() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());

    let (status, bytes) = call(&app, get(STIM)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value(&bytes), json!([]));

    let stim = active_stimulation("test1");
    let body = serde_json::to_value(&stim).unwrap();
    let (status, bytes) = call(&app, put_json(&format!("{STIM}/test1"), &body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let saved = value(&bytes);
    assert_eq!(saved["label"], "test1");
    assert_eq!(saved["contacts_per_side"], json!([4, 4]));
    let label_dir = subj_dir(root.path()).join("stimulations/native/test1");
    assert!(label_dir.join("sub-01_desc-stimulation.json").is_file());
    assert!(label_dir.join("sub-01_desc-stimparameters.mat").is_file());

    let (status, bytes) = call(&app, get(&format!("{STIM}/test1"))).await;
    assert_eq!(status, StatusCode::OK);
    let got = value(&bytes);
    assert_eq!(got["label"], "test1");
    assert_eq!(got["subject"], "01");
    assert_eq!(got["sides"]["right"]["sources"][0]["amplitude"], 2.0);
    let (_, bytes) = call(&app, get(STIM)).await;
    let rows = value(&bytes);
    assert_eq!(rows[0]["label"], "test1");
    assert_eq!(rows[0]["space"], "native");
    assert_eq!(rows[0]["state"], "canonical");

    let mut bad = stim.clone();
    bad.model_settings.threshold_v_per_mm = -1.0;
    let (status, bytes) = call(
        &app,
        put_json(
            &format!("{STIM}/test1"),
            &serde_json::to_value(&bad).unwrap(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(value(&bytes)["code"], "E_VAT_settings");
    let (_, bytes) = call(&app, get(&format!("{STIM}/test1"))).await;
    assert!(
        value(&bytes)["model_settings"]["threshold_v_per_mm"]
            .as_f64()
            .unwrap()
            > 0.0
    );

    let (status, bytes) = call(&app, put_json(&format!("{STIM}/other"), &body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_VAT_settings");

    let (status, _) = call(&app, get(&format!("{STIM}/missing"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&app, get(&format!("{STIM}/.hidden"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn group_labels_are_refused_for_subject_settings() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let body = serde_json::to_value(active_stimulation("gs_20261023")).unwrap();
    let (status, bytes) = call(&app, put_json(&format!("{STIM}/gs_20261023"), &body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let err = value(&bytes);
    assert_eq!(err["code"], "E_VAT_settings");
    assert!(err["message"].as_str().unwrap().contains("gs_"));
    assert!(!subj_dir(root.path())
        .join("stimulations/native/gs_20261023")
        .exists());
}

#[tokio::test]
async fn a_stimparameters_mat_upload_is_converted_through_the_pipeline() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let body = serde_json::to_value(active_stimulation("orig")).unwrap();
    let (status, _) = call(&app, put_json(&format!("{STIM}/orig"), &body)).await;
    assert_eq!(status, StatusCode::OK);
    let mat = std::fs::read(
        subj_dir(root.path()).join("stimulations/native/orig/sub-01_desc-stimparameters.mat"),
    )
    .unwrap();

    let (status, bytes) = call(
        &app,
        multipart(
            &format!("{STIM}/import"),
            &[
                ("label", None, b"copy1"),
                ("file", Some("sub-01_desc-stimparameters.mat"), &mat),
            ],
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(value(&bytes)["label"], "copy1");
    let (status, bytes) = call(&app, get(&format!("{STIM}/copy1"))).await;
    assert_eq!(status, StatusCode::OK);
    let got = value(&bytes);
    assert_eq!(got["sides"]["right"]["sources"][0]["amplitude"], 2.0);
    assert!(got["provenance"]["imported_from"]["sha256"].is_string());
    assert!(got["provenance"]["imported_from"]["path"].is_null());

    let (status, _) = call(
        &app,
        multipart(
            &format!("{STIM}/import"),
            &[("file", Some("x.mat"), b"not a mat file")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let leftovers = std::fs::read_dir(root.path().join("incoming"))
        .unwrap()
        .flatten()
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn a_preview_without_a_normalization_fails_cleanly_and_writes_nothing() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let body = serde_json::to_value(active_stimulation("test1")).unwrap();
    let (status, bytes) = call(
        &app,
        post_json(
            &format!("{STIM}/test1/preview"),
            &json!({ "stimulation": body }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(value(&bytes)["code"].as_str().unwrap().starts_with("E_VAT"));
    assert!(!subj_dir(root.path()).join("stimulations").exists());

    let (status, _) = call(
        &app,
        post_json(
            &format!("{STIM}/test1/preview"),
            &json!({ "stimulation": body, "backend": "tpu" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_requests_carry_the_vat_fields() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/runs",
            &json!({
                "stages": ["vat"],
                "vat_backend": "cpu",
                "vat_labels": ["stim_a", "stim_b"],
                "vat_stats_atlas": "DISTAL Minimal (Ewert 2017)",
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let id = value(&bytes)["id"].as_str().unwrap().to_owned();
    let job = state.db().get(&id).unwrap().unwrap();
    let opts: RunOptions = serde_json::from_str(&job.options).unwrap();
    assert_eq!(opts.vat_backend.as_str(), "cpu");
    assert_eq!(opts.vat_labels, vec!["stim_a", "stim_b"]);
    assert_eq!(
        opts.vat_stats_atlas.as_deref(),
        Some("DISTAL Minimal (Ewert 2017)")
    );
    assert!(opts.headless);
}

#[tokio::test]
async fn invalid_vat_fields_are_refused() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    for body in [
        json!({"vat_backend": "tpu"}),
        json!({"vat_labels": ["gs_1"]}),
        json!({"vat_labels": ["../x"]}),
        json!({"vat_stats_atlas": "../../etc"}),
    ] {
        let (status, bytes) = call(
            &app,
            post_json("/api/v1/datasets/demo/subjects/01/runs", &body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(value(&bytes)["code"], "E_BAD_REQUEST");
    }
}

#[tokio::test]
async fn the_allowlist_serves_stimulation_results_but_not_the_head_model_cache() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let sub = subj_dir(root.path());
    let label = sub.join("stimulations/MNI152NLin2009bAsym/test1");
    std::fs::create_dir_all(&label).unwrap();
    for name in ["a.nii", "b.nii.gz", "c.json", "d.mat", "e.bin", "f.txt"] {
        std::fs::write(label.join(name), b"x").unwrap();
    }
    std::fs::create_dir_all(sub.join("headmodel")).unwrap();
    std::fs::write(sub.join("headmodel/basis.bin"), b"secret").unwrap();
    std::fs::create_dir_all(sub.join("stimulations/native/test1/headmodel")).unwrap();
    std::fs::write(
        sub.join("stimulations/native/test1/headmodel/basis.json"),
        b"{}",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("datasets/demo/connectomes")).unwrap();
    std::fs::write(root.path().join("datasets/demo/connectomes/data.mat"), b"c").unwrap();

    let base = "/api/v1/datasets/demo/subjects/01/files/derivatives/leaddbs/sub-01";
    for ok in ["a.nii", "b.nii.gz", "c.json", "d.mat"] {
        let (status, _) = call(
            &app,
            get(&format!(
                "{base}/stimulations/MNI152NLin2009bAsym/test1/{ok}"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ok}");
    }
    for denied in [
        "stimulations/MNI152NLin2009bAsym/test1/e.bin",
        "stimulations/MNI152NLin2009bAsym/test1/f.txt",
        "headmodel/basis.bin",
        "stimulations/native/test1/headmodel/basis.json",
    ] {
        let (status, bytes) = call(&app, get(&format!("{base}/{denied}"))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
    }
    let (status, _) = call(
        &app,
        get("/api/v1/datasets/demo/subjects/01/files/connectomes/data.mat"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, get("/api/v1/connectomes/x/files/data.mat")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

const GROUPS: &str = "/api/v1/datasets/demo/groups";

async fn create_group(app: &Router, guid: &str) -> Value {
    let (status, bytes) = call(
        app,
        post_json(GROUPS, &json!({"guid": guid, "title": "cohort"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    value(&bytes)
}

fn group_folder(root: &Path, guid: &str) -> PathBuf {
    root.join("datasets/demo/derivatives/leadgroup").join(guid)
}

#[tokio::test]
async fn group_records_are_created_listed_read_and_replaced_with_relative_twins() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let created = create_group(&app, "g1").await;
    assert_eq!(created["guid"], "g1");
    let folder = group_folder(root.path(), "g1");
    assert!(folder.join("dataset-demo_analysis-g1.json").is_file());
    assert!(folder.join("dataset-demo_analysis-g1.mat").is_file());

    let (status, bytes) = call(&app, post_json(GROUPS, &json!({"guid": "g1"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_GROUP_EXISTS");
    let (status, _) = call(&app, post_json(GROUPS, &json!({"guid": "../x"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, bytes) = call(&app, get(GROUPS)).await;
    let rows = value(&bytes);
    assert_eq!(rows[0]["guid"], "g1");
    assert_eq!(rows[0]["patients"], 0);

    let (status, bytes) = call(&app, get(&format!("{GROUPS}/g1"))).await;
    assert_eq!(status, StatusCode::OK);
    let mut record = value(&bytes);
    assert_eq!(record["title"], "cohort");
    assert_eq!(record["datasets"][0]["root"], "");
    assert_eq!(
        record["datasets"][0]["derivatives_root"],
        "derivatives/leaddbs"
    );
    let root_text = root.path().to_string_lossy().replace('\\', "/");
    assert!(!String::from_utf8_lossy(&bytes)
        .replace("\\\\", "/")
        .contains(&root_text));

    record["title"] = json!("renamed");
    record["patients"] = json!([{"subject": "01", "dataset": "d1", "group": 1, "included": true}]);
    record["groups"] = json!([{"number": 1, "label": "responders"}]);
    let (status, bytes) = call(&app, put_json(&format!("{GROUPS}/g1"), &record)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let put = value(&bytes);
    assert_eq!(put["record"]["patients"][0]["subject"], "sub-01");
    assert_eq!(put["record"]["title"], "renamed");

    let twin = folder.join("dataset-demo_analysis-g1.mat");
    let read = dbsloc_pipeline::group::read_m(&twin).unwrap();
    assert_eq!(read.record.patients.len(), 1);
    for d in &read.record.datasets {
        assert!(
            !d.derivatives_root.contains(&root_text) && !d.derivatives_root.starts_with('/'),
            "twin path {:?} is not dataset-relative",
            d.derivatives_root
        );
        assert!(!d.derivatives_root.contains(':'));
    }
    let stored = std::fs::read_to_string(folder.join("dataset-demo_analysis-g1.json")).unwrap();
    assert!(stored.contains("sub-01"));

    let mut wrong_guid = record.clone();
    wrong_guid["guid"] = json!("g2");
    let (status, _) = call(&app, put_json(&format!("{GROUPS}/g1"), &wrong_guid)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut wrong_name = record.clone();
    wrong_name["dataset_name"] = json!("other");
    let (status, _) = call(&app, put_json(&format!("{GROUPS}/g1"), &wrong_name)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for bad in ["/etc", "../outside", "C:/x", "a\\b"] {
        let mut escaping = record.clone();
        escaping["datasets"][0]["derivatives_root"] = json!(bad);
        let (status, bytes) = call(&app, put_json(&format!("{GROUPS}/g1"), &escaping)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
    }
    let mut ghost = record.clone();
    ghost["patients"][0]["dataset"] = json!("nope");
    let (status, _) = call(&app, put_json(&format!("{GROUPS}/g1"), &ghost)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(&app, get(&format!("{GROUPS}/missing"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_clinical_table_upload_imports_variables_and_is_audit_logged() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    create_group(&app, "g1").await;
    let (_, bytes) = call(&app, get(&format!("{GROUPS}/g1"))).await;
    let mut record = value(&bytes);
    record["patients"] =
        json!([{"subject": "sub-01", "dataset": "d1", "group": 1, "included": true}]);
    let (status, _) = call(&app, put_json(&format!("{GROUPS}/g1"), &record)).await;
    assert_eq!(status, StatusCode::OK);

    let table = b"participant_id\tUPDRS\nsub-01\t12.5\nsub-99\t3\n";
    let (status, bytes) = call(
        &app,
        multipart(
            &format!("{GROUPS}/g1/clinical"),
            &[("file", Some("scores.tsv"), table)],
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let report = value(&bytes);
    assert_eq!(report["imported"], json!(["UPDRS"]));
    assert_eq!(report["unknown_subjects"], json!(["sub-99"]));

    let (_, bytes) = call(&app, get(&format!("{GROUPS}/g1"))).await;
    assert_eq!(value(&bytes)["clinical"][0]["name"], "UPDRS");

    let (status, bytes) = call(
        &app,
        multipart(
            &format!("{GROUPS}/g1/clinical"),
            &[("file", Some("scores.tsv"), table)],
        ),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let (status, _) = call(
        &app,
        multipart(
            &format!("{GROUPS}/g1/clinical?replace=true"),
            &[("file", Some("scores.tsv"), table)],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let audit = std::fs::read_to_string(root.path().join("server/audit.log")).unwrap();
    let entries: Vec<Value> = audit
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let upload = entries
        .iter()
        .find(|e| e["action"] == "clinical_upload")
        .expect("the upload is in the audit log");
    assert_eq!(upload["dataset"], "demo");
    assert!(upload["detail"].as_str().unwrap().contains("guid=g1"));
    assert!(entries
        .iter()
        .all(|e| !e["detail"].as_str().unwrap_or_default().contains("12.5")));
    assert!(entries.iter().any(|e| e["action"] == "group_create"));
    assert!(entries.iter().any(|e| e["action"] == "group_put"));

    let (status, bytes) = call(
        &app,
        get(&format!("{GROUPS}/g1/files/dataset-demo_analysis-g1.json")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&bytes).contains("UPDRS"));
    let (status, _) = call(&app, get(&format!("{GROUPS}/g1/files/../../../../secret"))).await;
    assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn group_job_requests_are_validated_and_one_job_runs_per_group() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    create_group(&app, "g1").await;
    let jobs = format!("{GROUPS}/g1/jobs");

    for body in [
        json!({"kind": "bogus"}),
        json!({"kind": "sweetspot", "options": {}}),
        json!({"kind": "group_calc", "options": {"mask": "grey"}}),
        json!({"kind": "netmap", "options": {"variable": "v", "connectome": "/data/c"}}),
        json!({"kind": "netmap", "options": {"variable": "v", "connectome": "GSP", "mask": "/m.nii"}}),
    ] {
        let (status, bytes) = call(&app, post_json(&jobs, &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(value(&bytes)["code"], "E_BAD_REQUEST");
    }
    let (status, _) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/groups/nope/jobs",
            &json!({"kind": "export"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, bytes) = call(
        &app,
        post_json(
            &jobs,
            &json!({"kind": "group_calc", "options": {"mirror": false}}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let id = value(&bytes)["id"].as_str().unwrap().to_owned();
    let (status, bytes) = call(&app, post_json(&jobs, &json!({"kind": "export"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_RUN_ACTIVE");
    let (_, bytes) = call(&app, get(&format!("{jobs}/active"))).await;
    assert_eq!(value(&bytes)["id"], id.as_str());

    let (status, _) = call(
        &app,
        put_json(
            &format!("{GROUPS}/g1"),
            &value(&call(&app, get(&format!("{GROUPS}/g1"))).await.1),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("{jobs}/{id}/cancel"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let status = poll_until(&app, &format!("{jobs}/{id}"), |v| v["phase"] == "cancelled").await;
    assert_eq!(status["phase"], "cancelled");
    let (status, _) = call(&app, get(&format!("{jobs}/nonexistent"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        get(&format!("/api/v1/datasets/demo/groups/g2/jobs/{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_group_job_runs_through_the_queue_with_the_fake_runner() {
    let root = scratch_root();
    let (state, app) = live_app(root.path(), fake_runner());
    create_group(&app, "g1").await;
    let jobs = format!("{GROUPS}/g1/jobs");
    let (status, bytes) = call(&app, post_json(&jobs, &json!({"kind": "export"}))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = value(&bytes)["id"].as_str().unwrap().to_owned();
    let done = poll_until(&app, &format!("{jobs}/{id}"), |v| v["phase"] == "done").await;
    assert_eq!(done["stages_ok"], 1);
    let resp = app
        .clone()
        .oneshot(get(&format!("{jobs}/{id}/events")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let text =
        String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(text.contains("stage_started"), "{text}");
    assert!(text.contains("event: end"), "{text}");
    let (_, bytes) = call(&app, get(&format!("{jobs}/active"))).await;
    assert!(value(&bytes).is_null());
    let audit = std::fs::read_to_string(root.path().join("server/audit.log")).unwrap();
    assert!(audit.contains("group_job_submit"));
    state.shutdown();
}

#[tokio::test]
async fn a_group_calc_job_on_a_subject_without_outputs_fails_cleanly() {
    let Some(runner) = built_runner() else {
        eprintln!("dbsloc_pipeline_runner is not built next to the tests; skipping");
        return;
    };
    let root = scratch_root();
    let (state, app) = live_app(root.path(), runner);
    create_group(&app, "g1").await;
    let (_, bytes) = call(&app, get(&format!("{GROUPS}/g1"))).await;
    let mut record = value(&bytes);
    record["patients"] =
        json!([{"subject": "sub-01", "dataset": "d1", "group": 1, "included": true}]);
    let (status, _) = call(&app, put_json(&format!("{GROUPS}/g1"), &record)).await;
    assert_eq!(status, StatusCode::OK);

    let jobs = format!("{GROUPS}/g1/jobs");
    let (status, bytes) = call(
        &app,
        post_json(
            &jobs,
            &json!({"kind": "group_calc", "options": {"vat_backend": "cpu"}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = value(&bytes)["id"].as_str().unwrap().to_owned();
    let end = poll_until(&app, &format!("{jobs}/{id}"), |v| {
        matches!(v["phase"].as_str(), Some("done" | "failed" | "cancelled"))
    })
    .await;
    assert_eq!(end["phase"], "failed", "{end}");
    assert_eq!(end["stages_failed"], 1);
    let root_text = root.path().to_string_lossy().replace('\\', "/");
    assert!(!end.to_string().replace("\\\\", "/").contains(&root_text));
    let resp = app
        .clone()
        .oneshot(get(&format!("{jobs}/{id}/events")))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes())
        .replace("\\\\", "/");
    assert!(text.contains("sub-01"), "{text}");
    assert!(!text.contains(&root_text), "{text}");

    let folder = group_folder(root.path(), "g1");
    assert!(folder.join("dataset-demo_analysis-g1.mat").is_file());
    let twin = folder.join("dataset-demo_analysis-g1.mat");
    let mut read = dbsloc_pipeline::group::read_m(&twin).unwrap();
    for _ in 0..200 {
        if read
            .record
            .datasets
            .iter()
            .all(|d| !d.derivatives_root.contains(&root_text))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(next) = dbsloc_pipeline::group::read_m(&twin) {
            read = next;
        }
    }
    for d in &read.record.datasets {
        assert!(
            !d.derivatives_root.contains(&root_text),
            "{} contains {root_text}",
            d.derivatives_root
        );
    }
    state.shutdown();
}

#[tokio::test]
async fn connectome_listing_hides_paths_and_downloads_are_off_by_default() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let (status, bytes) = call(&app, get("/api/v1/connectomes")).await;
    assert_eq!(status, StatusCode::OK);
    let v = value(&bytes);
    assert_eq!(v["download_allowed"], false);
    assert!(v["transfer"].is_null());
    let catalogue = v["catalogue"].as_array().unwrap();
    assert!(!catalogue.is_empty());
    let first = &catalogue[0];
    assert!(first["licence_text_sha256"].as_str().unwrap().len() == 64);
    assert_eq!(first["state"], "available");
    let root_text = root.path().to_string_lossy().replace('\\', "/");
    assert!(!String::from_utf8_lossy(&bytes)
        .replace("\\\\", "/")
        .contains(&root_text));

    let id = first["id"].as_str().unwrap();
    let (status, bytes) = call(
        &app,
        post_json(
            &format!("/api/v1/connectomes/{id}/fetch"),
            &json!({"accepted_by": "me", "licence_text_sha256": first["licence_text_sha256"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&bytes)["code"], "E_CONN_DISABLED");
    assert!(state.transfer_slot().is_none());

    let sanitized = state
        .sanitizer(None)
        .text(&format!(
            "{}/open/x.trk",
            root.path().join("connectomes").to_string_lossy()
        ))
        .into_owned();
    assert!(sanitized.starts_with("<connectomes>"), "{sanitized}");
}

#[tokio::test]
async fn an_enabled_fetch_checks_the_request_before_any_transfer() {
    let root = scratch_root();
    let mut cfg = config(root.path(), fake_runner());
    cfg.allow_connectome_download = true;
    let state = AppState::new_idle(cfg).unwrap();
    let app = build_router(state.clone());
    let (_, bytes) = call(&app, get("/api/v1/connectomes")).await;
    let v = value(&bytes);
    assert_eq!(v["download_allowed"], true);
    let first = &v["catalogue"][0];
    let id = first["id"].as_str().unwrap();
    let uri = format!("/api/v1/connectomes/{id}/fetch");

    let (status, bytes) = call(
        &app,
        post_json(
            &uri,
            &json!({"accepted_by": "me", "licence_text_sha256": "0".repeat(64)}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_CONN_TERMS");
    let (status, _) = call(
        &app,
        post_json(
            &uri,
            &json!({"accepted_by": " ", "licence_text_sha256": first["licence_text_sha256"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        post_json(
            "/api/v1/connectomes/nonexistent/fetch",
            &json!({"accepted_by": "me", "licence_text_sha256": "x"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let dua = v["catalogue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["access"] == "dua")
        .unwrap();
    let (status, bytes) = call(
        &app,
        post_json(
            &format!("/api/v1/connectomes/{}/fetch", dua["id"].as_str().unwrap()),
            &json!({
                "accepted_by": "me",
                "licence_text_sha256": dua["licence_text_sha256"],
                "attest": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let refusal = value(&bytes);
    assert_eq!(refusal["code"], "E_CONN_TERMS");
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("is import-only"),
        "{refusal}"
    );
    assert!(state.transfer_slot().is_none());
    let (status, _) = call(&app, get(&format!("/api/v1/connectomes/{id}/fetch/events"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/connectomes/{id}/fetch/cancel"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
