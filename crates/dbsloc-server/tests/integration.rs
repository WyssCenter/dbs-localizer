// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests for the `dbsloc-server` router (WP2 + WP3), Windows-friendly and GPU-free.
//!
//! The pipeline is never executed: the single run test points `DBSLOC_PIPELINE_RUNNER_BIN` at a tiny
//! fake runner (`.cmd` on Windows, `.sh` elsewhere) that emits two `RunEvent` lines and exits 0.
//! Every other assertion is read-only or exercises upload staging, NIfTI validation and approvals.

// automatically cover them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
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

fn app(root: &std::path::Path) -> (AppState, Router) {
    std::env::set_var("WGPU_FORCE_FALLBACK_ADAPTER", "1");
    let mut config = Config::default_for_root(root);
    config.templates_dir = Some(root.join("templates"));
    config.pipeline_runner_bin = Some(runner_bin());
    let state = AppState::new(config).unwrap();
    let router = build_router(state.clone());
    (state, router)
}

fn idle_app(root: &std::path::Path) -> (AppState, Router) {
    let mut config = Config::default_for_root(root);
    config.templates_dir = Some(root.join("templates"));
    config.pipeline_runner_bin = Some(runner_bin());
    let state = AppState::new_idle(config).unwrap();
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

fn post_json(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn put_bytes(uri: &str, bytes: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .body(Body::from(bytes))
        .unwrap()
}

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not JSON: {e}: {}", String::from_utf8_lossy(bytes)))
}

/// A minimal valid NIfTI-1 volume (2x2x2 uint8, qform set) with `descrip` text to scrub.
fn minimal_nifti() -> Vec<u8> {
    let mut b = vec![0u8; 352 + 8];
    b[0..4].copy_from_slice(&348i32.to_le_bytes());
    b[40..42].copy_from_slice(&3i16.to_le_bytes());
    b[42..44].copy_from_slice(&2i16.to_le_bytes());
    b[44..46].copy_from_slice(&2i16.to_le_bytes());
    b[46..48].copy_from_slice(&2i16.to_le_bytes());
    for i in 4..8 {
        b[40 + 2 * i..42 + 2 * i].copy_from_slice(&1i16.to_le_bytes());
    }
    b[70..72].copy_from_slice(&2i16.to_le_bytes());
    b[72..74].copy_from_slice(&8i16.to_le_bytes());
    for i in 1..4 {
        b[76 + 4 * i..80 + 4 * i].copy_from_slice(&1.0f32.to_le_bytes());
    }
    b[108..112].copy_from_slice(&352.0f32.to_le_bytes());
    b[148..159].copy_from_slice(b"PATIENTNAME");
    b[228..234].copy_from_slice(b"AUSCAN");
    b[252..254].copy_from_slice(&1i16.to_le_bytes());
    b[344..348].copy_from_slice(b"n+1\0");
    b
}

fn sha256_hex(data: &[u8]) -> String {
    dbsloc_pipeline::sha256::sha256_hex(data)
}

fn runner_bin() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    if cfg!(windows) {
        dir.join("fake_runner.cmd")
    } else {
        dir.join("fake_runner.sh")
    }
}

async fn poll_until<F: Fn(&Value) -> bool>(app: &Router, uri: &str, done: F) -> Value {
    for _ in 0..100 {
        let (_, bytes) = call(app, get(uri)).await;
        let v = value(&bytes);
        if done(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out polling {uri}");
}

#[tokio::test]
async fn api_end_to_end() {
    let root = scratch_root();
    let (state, app) = app(root.path());

    let (status, bytes) = call(&app, get("/api/v1/gpu")).await;
    assert_eq!(status, StatusCode::OK);
    let gpu = value(&bytes);
    assert!(gpu.get("adapter").is_some());
    assert!(gpu.get("verdict").is_some());

    let (status, bytes) = call(&app, get("/api/v1/datasets")).await;
    assert_eq!(status, StatusCode::OK);
    let ds = value(&bytes);
    assert_eq!(ds[0]["name"], "demo");
    assert_eq!(ds[0]["subjects"], 1);

    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value(&bytes), json!(["01"]));

    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01")).await;
    assert_eq!(status, StatusCode::OK);
    let summary = value(&bytes);
    assert_eq!(summary["subj_id"], "01");
    assert_eq!(summary["anchor_modality"], "T1w");
    assert!(summary["stages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["stage"] == "preprocess"));

    let (status, bytes) = call(
        &app,
        post_json("/api/v1/datasets/demo/subjects/01/plan", &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let plan = value(&bytes);
    assert!(plan["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["stage"] == "preprocess"));
    assert_eq!(plan["stages"][0], "bids");

    let file_uri = "/api/v1/datasets/demo/subjects/01/files/rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_T1w.nii.gz";
    let (status, bytes) = call(&app, get(file_uri)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes, b"hello");
    let mut req = get(file_uri);
    req.headers_mut()
        .insert("range", "bytes=1-3".parse().unwrap());
    let (status, bytes) = call(&app, req).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, b"ell");

    let (status, _) = call(
        &app,
        get("/api/v1/datasets/demo/subjects/01/files/%2e%2e%2fsecret"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        get("/api/v1/datasets/demo/subjects/01/files/%2Fetc%2Fpasswd"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = call(
        &app,
        get("/api/v1/datasets/demo/subjects/01/files/rawdata/sub-02/anat/sub-02_T1w.nii.gz"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        get("/api/v1/datasets/demo/subjects/01/files/runs/deadbeef.ndjson"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let nifti = minimal_nifti();
    let digest = sha256_hex(&nifti);
    let init = json!({"files": [{"name": "ct.nii", "size": nifti.len(), "sha256": digest}]});
    let (status, bytes) = call(&app, post_json("/api/v1/datasets/demo/uploads", &init)).await;
    assert_eq!(status, StatusCode::OK);
    let init_resp = value(&bytes);
    let upload_id = init_resp["upload_id"].as_str().unwrap().to_owned();
    assert_eq!(init_resp["chunk_size"].as_u64().unwrap(), 8 * 1024 * 1024);

    let (status, _) = call(
        &app,
        put_bytes(
            &format!("/api/v1/uploads/{upload_id}/ct.nii/0"),
            nifti.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        put_bytes(
            &format!("/api/v1/uploads/{upload_id}/ct.nii/0"),
            nifti.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{upload_id}/convert"), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value(&bytes), json!([]));

    let commit = json!({
        "subject": "01",
        "mapping": [{"file": "ct.nii", "session": "postop", "modality": "anat", "suffix": "CT"}]
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{upload_id}/commit"), &commit),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "commit failed: {}",
        String::from_utf8_lossy(&bytes)
    );
    let placed = root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii.gz");
    assert!(placed.is_file());
    let scrubbed = gunzip(&std::fs::read(&placed).unwrap());
    assert!(scrubbed[148..228].iter().all(|&b| b == 0));
    assert!(scrubbed[228..252].iter().all(|&b| b == 0));
    let rawimages: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.path()
                .join("datasets/demo/rawdata/sub-01/rawimages.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(rawimages
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["entities"]["suffix"] == "CT"));

    let init2 =
        json!({"files": [{"name": "ct.nii", "size": nifti.len(), "sha256": sha256_hex(&nifti)}]});
    let (_, bytes) = call(&app, post_json("/api/v1/datasets/demo/uploads", &init2)).await;
    let upload2 = value(&bytes)["upload_id"].as_str().unwrap().to_owned();
    let _ = call(
        &app,
        put_bytes(
            &format!("/api/v1/uploads/{upload2}/ct.nii/0"),
            nifti.clone(),
        ),
    )
    .await;
    let commit2 = json!({
        "subject": "01",
        "mapping": [{"file": "ct.nii", "session": "postop", "modality": "anat", "suffix": "CT"}]
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{upload2}/commit"), &commit2),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(value(&bytes)["code"], "E_IMPORT_CONFLICT");
    let commit2 = json!({
        "subject": "01",
        "mapping": [{"file": "ct.nii", "session": "postop", "modality": "anat", "suffix": "CT"}],
        "replace_existing": true
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{upload2}/commit"), &commit2),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "re-commit failed: {}",
        String::from_utf8_lossy(&bytes)
    );
    let rawimages: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.path()
                .join("datasets/demo/rawdata/sub-01/rawimages.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let ct_rows = rawimages
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["entities"]["suffix"] == "CT")
        .count();
    assert_eq!(ct_rows, 1);

    let (status, _) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/locks/preprocess",
            &json!({"approved": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let lock = root
        .path()
        .join("datasets/demo/derivatives/leaddbs/sub-01/locks/preprocess.json");
    let lock_json: Value = serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    assert_eq!(lock_json["approval"], json!(true));

    let subj_dir = root.path().join("datasets/demo/derivatives/leaddbs/sub-01");
    let log = subj_dir.join("coregistration/log/sub-01_desc-coregmethod.json");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"{}").unwrap();
    let ct = subj_dir
        .join("coregistration/anat/sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    std::fs::write(&ct, b"x").unwrap();
    let manifest = json!({
        "slides": [{
            "checkreg_png": subj_dir.join("locks/x.png").to_string_lossy(),
            "moving": ct.to_string_lossy(),
            "fixed": ct.to_string_lossy(),
            "branch": "coreg",
            "modality": "CT",
            "method_log": log.to_string_lossy(),
        }]
    });
    std::fs::write(
        subj_dir.join("locks/checkreg_manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/qc/checkreg")).await;
    assert_eq!(status, StatusCode::OK);
    let qc = value(&bytes);
    assert_eq!(qc["slides"][0]["modality"], "CT");
    assert_eq!(qc["slides"][0]["approved"], json!(false));
    assert!(qc["slides"][0]["moving"]
        .as_str()
        .unwrap()
        .starts_with("derivatives/"));

    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/qc/checkreg/0/approval",
            &json!({"ok": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let approval = value(&bytes);
    assert_eq!(approval["all_approved"], json!(true));
    assert!(approval.get("resumed_run").is_none());
    let log_json: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    assert_eq!(log_json["approval"], json!({"CT": 1}));
    let (_, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/qc/checkreg")).await;
    assert_eq!(value(&bytes)["slides"][0]["approved"], json!(true));

    let (status, bytes) = call(
        &app,
        post_json("/api/v1/datasets/demo/subjects/01/runs", &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = value(&bytes)["id"].as_str().unwrap().to_owned();

    let status_json = poll_until(&app, &format!("/api/v1/runs/{run_id}"), |v| {
        v["phase"] == "done" || v["phase"] == "failed"
    })
    .await;
    assert_eq!(status_json["phase"], "done", "{status_json}");
    assert_eq!(status_json["stages_ok"], 1);

    let ndjson = state.run_ndjson("demo", &run_id);
    let text = std::fs::read_to_string(&ndjson).unwrap();
    assert!(text.contains("stage_started"));
    assert!(text.contains("pipeline_completed"));

    let resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/runs/{run_id}/events")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let collected = tokio::time::timeout(Duration::from_secs(5), resp.into_body().collect())
        .await
        .expect("the stream of a finished run must end")
        .unwrap();
    let sse = String::from_utf8_lossy(&collected.to_bytes()).into_owned();
    assert!(sse.contains("stage_started"), "SSE body: {sse}");
    assert!(sse.contains("pipeline_completed"), "SSE body: {sse}");
    assert!(sse.contains("event: end"), "SSE body: {sse}");

    let original =
        dbsloc_pipeline::options::RunOptions::new(root.path().join("datasets/demo"), "01");
    let original_id = dbsloc_server::runs::new_id();
    state
        .db()
        .insert_as(
            &dbsloc_server::db::NewJob {
                id: original_id.clone(),
                dataset: "demo".to_owned(),
                subject: "01".to_owned(),
                options: serde_json::to_string(&original).unwrap(),
                priority: 1,
                resume: false,
                gated_stage: Some("normalize".to_owned()),
                client: Some("test".to_owned()),
            },
            "awaiting_qc",
        )
        .unwrap();
    let awaiting = state
        .db()
        .awaiting_qc_for_subject("demo", "01")
        .unwrap()
        .unwrap();
    assert_eq!(awaiting.gated_stage.as_deref(), Some("normalize"));
    let resumed = match dbsloc_server::runs::submit_resume(&state, "demo", "01", &awaiting, "test")
        .unwrap()
    {
        dbsloc_server::runs::ResumeOutcome::Resumed(id) => id,
        other => panic!("expected a resume id, got {other:?}"),
    };
    let resume_job = state.db().get(&resumed).unwrap().unwrap();
    assert!(resume_job.resume);
    assert_eq!(resume_job.priority, 0, "resume jobs go to the head");
    let resume_opts: dbsloc_pipeline::options::RunOptions =
        serde_json::from_str(&resume_job.options).unwrap();
    assert_eq!(
        resume_opts.stages,
        vec![
            dbsloc_pipeline::stages::Stage::Brainshift,
            dbsloc_pipeline::stages::Stage::Reconstruct,
            dbsloc_pipeline::stages::Stage::Orientation,
            dbsloc_pipeline::stages::Stage::Vat,
            dbsloc_pipeline::stages::Stage::Visualize,
        ]
    );
}

#[tokio::test]
async fn commit_rejects_a_missing_middle_chunk() {
    let root = scratch_root();
    let mut config = Config::default_for_root(root.path());
    config.templates_dir = Some(root.path().join("templates"));
    config.chunk_size = 16;
    config.pipeline_runner_bin = Some(runner_bin());
    let state = AppState::new(config).unwrap();
    let app = build_router(state.clone());

    let data = vec![7u8; 48];
    let digest = sha256_hex(&data);
    let init = json!({"files": [{"name": "a.nii", "size": data.len(), "sha256": digest}]});
    let (status, bytes) = call(&app, post_json("/api/v1/datasets/demo/uploads", &init)).await;
    assert_eq!(status, StatusCode::OK);
    let id = value(&bytes)["upload_id"].as_str().unwrap().to_owned();

    let (status, _) = call(
        &app,
        put_bytes(
            &format!("/api/v1/uploads/{id}/a.nii/0"),
            data[0..16].to_vec(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &app,
        put_bytes(
            &format!("/api/v1/uploads/{id}/a.nii/2"),
            data[32..48].to_vec(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let commit = json!({
        "subject": "01",
        "mapping": [{"file": "a.nii", "session": "postop", "modality": "anat", "suffix": "T2w"}]
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{id}/commit"), &commit),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(value(&bytes)["code"], "E_UPLOAD");
}

async fn announce(app: &Router, files: &[(&str, &[u8])]) -> String {
    let list: Vec<Value> = files
        .iter()
        .map(|(name, data)| json!({"name": name, "size": data.len(), "sha256": sha256_hex(data)}))
        .collect();
    let (status, bytes) = call(
        app,
        post_json("/api/v1/datasets/demo/uploads", &json!({ "files": list })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    value(&bytes)["upload_id"].as_str().unwrap().to_owned()
}

fn converted_json(file: &str) -> Vec<u8> {
    serde_json::to_vec(&json!([{
        "file": file,
        "dims": [2, 2, 2],
        "mapping": {"file": file, "session": "postop", "modality": "anat", "suffix": "T2w", "acq": "ax"}
    }]))
    .unwrap()
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

async fn commit_mapping(app: &Router, id: &str, mapping: Value) -> (StatusCode, Value) {
    let (status, bytes) = call(
        app,
        post_json(
            &format!("/api/v1/uploads/{id}/commit"),
            &json!({"subject": "01", "mapping": mapping}),
        ),
    )
    .await;
    (status, value(&bytes))
}

#[tokio::test]
async fn converted_nifti_is_committed_from_the_nifti_dir() {
    let root = scratch_root();
    let (state, app) = app(root.path());

    let dicom = vec![5u8; 64];
    let id = announce(&app, &[("IM0001.dcm", &dicom[..])]).await;
    let (status, _) = call(
        &app,
        put_bytes(&format!("/api/v1/uploads/{id}/IM0001.dcm/0"), dicom.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let nifti = minimal_nifti();
    let upload_dir = state.config().incoming_dir().join(&id);
    std::fs::create_dir_all(upload_dir.join("nifti")).unwrap();
    std::fs::write(upload_dir.join("nifti/series.nii"), &nifti).unwrap();
    std::fs::write(
        upload_dir.join("nifti/series.json"),
        br#"{"SeriesDescription":"x"}"#,
    )
    .unwrap();
    std::fs::write(
        upload_dir.join("converted.json"),
        converted_json("series.nii"),
    )
    .unwrap();

    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([{"file": "series.nii", "session": "postop", "modality": "anat", "suffix": "T2w", "acq": "ax"}]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "commit failed: {body}");
    let anat = root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop/anat");
    let placed = anat.join("sub-01_ses-postop_acq-ax_T2w.nii.gz");
    assert!(placed.is_file(), "converted NIfTI was not placed");
    assert!(anat.join("sub-01_ses-postop_acq-ax_T2w.json").is_file());
    let scrubbed = gunzip(&std::fs::read(&placed).unwrap());
    assert!(scrubbed[148..228].iter().all(|&b| b == 0));
}

#[tokio::test]
async fn commit_refuses_an_unconverted_unannounced_file() {
    let root = scratch_root();
    let (state, app) = app(root.path());

    let dicom = [5u8; 64];
    let id = announce(&app, &[("IM0001.dcm", &dicom[..])]).await;
    let upload_dir = state.config().incoming_dir().join(&id);
    std::fs::create_dir_all(upload_dir.join("nifti")).unwrap();
    std::fs::write(upload_dir.join("nifti/series.nii"), minimal_nifti()).unwrap();
    std::fs::write(
        upload_dir.join("converted.json"),
        converted_json("series.nii"),
    )
    .unwrap();
    std::fs::write(upload_dir.join("nifti/other.nii"), minimal_nifti()).unwrap();

    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([{"file": "other.nii", "session": "postop", "modality": "anat", "suffix": "T2w"}]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "E_UPLOAD");
    assert!(!root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop")
        .exists());
}

#[tokio::test]
async fn convert_verifies_the_sha256_of_every_raw_file() {
    let root = scratch_root();
    let (_state, app) = app(root.path());

    let good = [5u8; 64];
    let id = announce(&app, &[("IM0001.dcm", &good[..])]).await;
    let (status, _) = call(
        &app,
        put_bytes(&format!("/api/v1/uploads/{id}/IM0001.dcm/0"), vec![6u8; 64]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{id}/convert"), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_UPLOAD");
}

#[tokio::test]
async fn chunks_must_match_the_announced_files() {
    let root = scratch_root();
    let mut config = Config::default_for_root(root.path());
    config.chunk_size = 16;
    config.pipeline_runner_bin = Some(runner_bin());
    let state = AppState::new(config).unwrap();
    let app = build_router(state.clone());

    let data = [7u8; 40];
    let id = announce(&app, &[("a.nii", &data[..])]).await;
    let put = |file: &str, chunk: u32, len: usize| {
        put_bytes(
            &format!("/api/v1/uploads/{id}/{file}/{chunk}"),
            vec![7u8; len],
        )
    };

    let (status, bytes) = call(&app, put("other.nii", 0, 16)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_UPLOAD");
    let (status, _) = call(&app, put("a.nii", 3, 16)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, put("a.nii", 0, 15)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, put("a.nii", 0, 17)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, put("a.nii", 2, 16)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for (chunk, len) in [(0, 16), (1, 16), (2, 8)] {
        let (status, _) = call(&app, put("a.nii", chunk, len)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "chunk {chunk}");
    }
    let dir = state.config().incoming_dir().join(&id).join("chunks");
    let leftovers = std::fs::read_dir(dir.join("a.nii"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn upload_init_rejects_an_empty_file() {
    let root = scratch_root();
    let mut config = Config::default_for_root(root.path());
    config.pipeline_runner_bin = Some(runner_bin());
    let state = AppState::new(config).unwrap();
    let app = build_router(state);
    let files = json!({"files": [{"name": "empty.nii", "size": 0, "sha256": sha256_hex(&[])}]});
    let (status, bytes) = call(&app, post_json("/api/v1/datasets/demo/uploads", &files)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_UPLOAD");
}

#[tokio::test]
async fn upload_init_rejects_a_traversal_dataset_name() {
    let root = scratch_root();
    let (state, app) = app(root.path());
    let data = minimal_nifti();
    let body =
        json!({"files": [{"name": "a.nii", "size": data.len(), "sha256": sha256_hex(&data)}]});
    for ds in ["%2E%2E", "%2e%2e", "a%3Ab"] {
        let (status, bytes) = call(
            &app,
            post_json(&format!("/api/v1/datasets/{ds}/uploads"), &body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{ds}");
        assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
    }

    let id = announce(&app, &[("a.nii", &data[..])]).await;
    let manifest_path = state.config().incoming_dir().join(&id).join("upload.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["dataset"] = json!("..");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([{"file": "a.nii", "session": "postop", "modality": "anat", "suffix": "CT"}]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "E_BAD_KEY");
}

#[tokio::test]
async fn commit_refuses_two_files_for_one_destination() {
    let root = scratch_root();
    let (_state, app) = app(root.path());

    let a = minimal_nifti();
    let mut b = minimal_nifti();
    b[360 - 1] = 9;
    let id = announce(&app, &[("a.nii", &a[..]), ("b.nii", &b[..])]).await;
    for (name, data) in [("a.nii", &a[..]), ("b.nii", &b[..])] {
        let (status, _) = call(
            &app,
            put_bytes(&format!("/api/v1/uploads/{id}/{name}/0"), data.to_vec()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([
            {"file": "a.nii", "session": "postop", "modality": "anat", "suffix": "CT"},
            {"file": "b.nii", "session": "postop", "modality": "anat", "suffix": "CT"},
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "E_UPLOAD");
    assert!(!root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop")
        .exists());
}

#[tokio::test]
async fn a_corrupt_rawimages_index_is_refused_not_discarded() {
    let root = scratch_root();
    let (_state, app) = app(root.path());
    let index = root
        .path()
        .join("datasets/demo/rawdata/sub-01/rawimages.json");
    std::fs::write(&index, b"{broken").unwrap();

    let data = minimal_nifti();
    let id = announce(&app, &[("a.nii", &data[..])]).await;
    let (status, _) = call(
        &app,
        put_bytes(&format!("/api/v1/uploads/{id}/a.nii/0"), data.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([{"file": "a.nii", "session": "postop", "modality": "anat", "suffix": "CT"}]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "E_NIFTI");
    assert!(body["message"].as_str().unwrap().contains("rawimages.json"));
    assert_eq!(std::fs::read(&index).unwrap(), b"{broken");
    assert!(!root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop")
        .exists());
}

#[tokio::test]
async fn a_duplicate_index_row_elsewhere_rolls_the_commit_back() {
    let root = scratch_root();
    let (_state, app) = app(root.path());
    let index = root
        .path()
        .join("datasets/demo/rawdata/sub-01/rawimages.json");
    let row = json!({
        "path": "rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_T1w.nii.gz",
        "session": "preop",
        "datatype": "anat",
        "entities": {"suffix": "T1w"},
    });
    let original = serde_json::to_string_pretty(&json!([row, row])).unwrap();
    std::fs::write(&index, &original).unwrap();

    let data = minimal_nifti();
    let id = announce(&app, &[("a.nii", &data[..])]).await;
    let (status, _) = call(
        &app,
        put_bytes(&format!("/api/v1/uploads/{id}/a.nii/0"), data.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = commit_mapping(
        &app,
        &id,
        json!([{"file": "a.nii", "session": "postop", "modality": "anat", "suffix": "CT"}]),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "E_DUPL_RAW");
    assert_eq!(std::fs::read_to_string(&index).unwrap(), original);
    assert!(!root
        .path()
        .join("datasets/demo/rawdata/sub-01/ses-postop/anat/sub-01_ses-postop_CT.nii")
        .exists());
}

#[tokio::test]
async fn commit_rejects_a_bad_modality_and_session() {
    let root = scratch_root();
    let (_state, app) = app(root.path());

    let nifti = minimal_nifti();
    let digest = sha256_hex(&nifti);
    let init = json!({"files": [{"name": "x.nii", "size": nifti.len(), "sha256": digest}]});
    let (_, bytes) = call(&app, post_json("/api/v1/datasets/demo/uploads", &init)).await;
    let id = value(&bytes)["upload_id"].as_str().unwrap().to_owned();
    let _ = call(
        &app,
        put_bytes(&format!("/api/v1/uploads/{id}/x.nii/0"), nifti.clone()),
    )
    .await;

    let commit = json!({
        "subject": "01",
        "mapping": [{"file": "x.nii", "session": "postop", "modality": "../../evil", "suffix": "T1w"}]
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{id}/commit"), &commit),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
    assert!(!root.path().join("evil").exists());
    assert!(!root.path().join("datasets/evil").exists());

    let commit = json!({
        "subject": "01",
        "mapping": [{"file": "x.nii", "session": "..", "modality": "anat", "suffix": "T1w"}]
    });
    let (status, bytes) = call(
        &app,
        post_json(&format!("/api/v1/uploads/{id}/commit"), &commit),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
}

#[tokio::test]
async fn qc_approval_retires_the_awaiting_job_and_dedupes_resume() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());

    let subj_dir = root.path().join("datasets/demo/derivatives/leaddbs/sub-01");
    let log = subj_dir.join("coregistration/log/sub-01_desc-coregmethod.json");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, br#"{"approval": true}"#).unwrap();
    let ct = subj_dir
        .join("coregistration/anat/sub-01_ses-postop_space-anchorNative_desc-preproc_CT.nii.gz");
    std::fs::create_dir_all(ct.parent().unwrap()).unwrap();
    std::fs::write(&ct, b"x").unwrap();
    std::fs::create_dir_all(subj_dir.join("locks")).unwrap();
    let manifest = json!({
        "slides": [{
            "checkreg_png": subj_dir.join("locks/x.png").to_string_lossy(),
            "moving": ct.to_string_lossy(),
            "fixed": ct.to_string_lossy(),
            "branch": "coreg",
            "modality": "CT",
            "method_log": log.to_string_lossy(),
        }]
    });
    std::fs::write(
        subj_dir.join("locks/checkreg_manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let opts = dbsloc_pipeline::options::RunOptions::new(root.path().join("datasets/demo"), "01");
    state
        .db()
        .insert_as(
            &dbsloc_server::db::NewJob {
                id: "orig".to_owned(),
                dataset: "demo".to_owned(),
                subject: "01".to_owned(),
                options: serde_json::to_string(&opts).unwrap(),
                priority: 1,
                resume: false,
                gated_stage: Some("coregister".to_owned()),
                client: Some("test".to_owned()),
            },
            "awaiting_qc",
        )
        .unwrap();

    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/qc/checkreg/0/approval",
            &json!({"ok": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = value(&bytes);
    assert_eq!(first["all_approved"], json!(true));
    let resumed_run = first["resumed_run"]
        .as_str()
        .expect("a resume run")
        .to_owned();

    let orig = state.db().get("orig").unwrap().unwrap();
    assert_eq!(orig.phase, "resumed");
    assert_eq!(orig.resumed_by.as_deref(), Some(resumed_run.as_str()));
    assert!(state
        .db()
        .awaiting_qc_for_subject("demo", "01")
        .unwrap()
        .is_none());

    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/qc/checkreg/0/approval",
            &json!({"ok": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(value(&bytes).get("resumed_run").is_none());
    assert!(state
        .db()
        .awaiting_qc_for_subject("demo", "01")
        .unwrap()
        .is_none());
}

fn root_forms(root: &std::path::Path) -> Vec<String> {
    let mut forms = vec![root.to_string_lossy().into_owned()];
    if let Ok(c) = std::fs::canonicalize(root) {
        forms.push(c.to_string_lossy().into_owned());
        forms.push(
            dbsloc_pipeline::bids::simplify_verbatim(c)
                .to_string_lossy()
                .into_owned(),
        );
    }
    let slashed: Vec<String> = forms.iter().map(|f| f.replace('\\', "/")).collect();
    forms.extend(slashed);
    forms.into_iter().map(|f| f.to_lowercase()).collect()
}

fn assert_no_root(what: &str, body: &[u8], root: &std::path::Path) {
    let text = String::from_utf8_lossy(body)
        .replace("\\\\", "\\")
        .to_lowercase();
    for form in root_forms(root) {
        assert!(
            !text.contains(&form),
            "{what} leaks the server path {form}: {text}"
        );
    }
}

#[tokio::test]
async fn no_response_or_event_stream_carries_an_absolute_server_path() {
    use dbsloc_pipeline::protocol::RunEvent;

    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let ds_root = root.path().join("datasets/demo");
    let subj = ds_root.join("derivatives/leaddbs/sub-01");
    let abs = |rel: &str| subj.join(rel).to_string_lossy().into_owned();

    state
        .db()
        .insert_as(
            &dbsloc_server::db::NewJob {
                id: "leak".to_owned(),
                dataset: "demo".to_owned(),
                subject: "01".to_owned(),
                options: "{}".to_owned(),
                priority: 1,
                resume: false,
                gated_stage: None,
                client: None,
            },
            "running",
        )
        .unwrap();
    let failure = format!(
        "normalize: cannot read {}: denied",
        abs("normalization/anat/x.nii.gz")
    );
    state
        .db()
        .finish("leak", "failed", 0, 1, 0, Some(&failure))
        .unwrap();
    let events = [
        RunEvent::StageStarted {
            stage: "normalize".to_owned(),
        },
        RunEvent::LogLine {
            line: format!("dbsloc: reading {}", abs("coregistration/anat")),
        },
        RunEvent::StageFailed {
            stage: "normalize".to_owned(),
            message: failure.clone(),
        },
    ];
    let ndjson = state.run_ndjson("demo", "leak");
    std::fs::create_dir_all(ndjson.parent().unwrap()).unwrap();
    let lines: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    std::fs::write(&ndjson, lines.join("\n") + "\n").unwrap();

    let (status, bytes) = call(&app, get("/api/v1/runs/leak")).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_root("run status", &bytes, root.path());
    let message = value(&bytes)["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("<datasets>/demo/derivatives/leaddbs/sub-01/normalization/anat/x.nii.gz"),
        "{message}"
    );
    assert_eq!(
        state.db().get("leak").unwrap().unwrap().message.as_deref(),
        Some(failure.as_str())
    );

    let resp = app
        .clone()
        .oneshot(get("/api/v1/runs/leak/events"))
        .await
        .unwrap();
    let sse = resp.into_body().collect().await.unwrap().to_bytes();
    let sse_text = String::from_utf8_lossy(&sse).into_owned();
    assert!(sse_text.contains("<datasets>/demo"), "{sse_text}");
    assert_no_root("event stream", &sse, root.path());

    let log = subj.join("normalization/log/sub-01_desc-normmethod.json");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, b"[1]").unwrap();
    std::fs::create_dir_all(subj.join("locks")).unwrap();
    std::fs::write(
        subj.join("locks/checkreg_manifest.json"),
        serde_json::to_vec(&json!({"slides": [{
            "checkreg_png": abs("locks/n.png"),
            "moving": abs("normalization/anat/x.nii.gz"),
            "fixed": abs("normalization/anat/x.nii.gz"),
            "branch": "norm",
            "modality": "T1w",
            "method_log": log.to_string_lossy(),
        }]}))
        .unwrap(),
    )
    .unwrap();
    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/qc/checkreg/0/approval",
            &json!({"ok": true}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(String::from_utf8_lossy(&bytes).contains("<datasets>/demo"));
    assert_no_root("approval error", &bytes, root.path());

    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/qc/checkreg")).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_root("checkreg manifest", &bytes, root.path());

    let (status, bytes) = call(
        &app,
        post_json("/api/v1/datasets/demo/subjects/01/plan", &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_no_root("plan", &bytes, root.path());

    let viz = subj.join("reconstruction/viz");
    std::fs::create_dir_all(&viz).unwrap();
    std::fs::write(viz.join("reco.json"), b"{}").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        viz.join("scene_manifest.json"),
        serde_json::to_vec(&json!({
            "subj_id": "01",
            "template_nii": root.path().join("templates/t1.nii.gz").to_string_lossy(),
            "reco_json": viz.join("reco.json").to_string_lossy(),
            "note": format!("built from {}", abs("reconstruction")),
            "later_field": {"paths": [outside.path().to_string_lossy(), abs("x")]},
        }))
        .unwrap(),
    )
    .unwrap();
    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/viz/scene")).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_root("scene manifest", &bytes, root.path());
    let scene = value(&bytes);
    assert_eq!(scene["template_nii"], "templates/t1.nii.gz");
    assert_eq!(
        scene["reco_json"],
        "derivatives/leaddbs/sub-01/reconstruction/viz/reco.json"
    );
    assert_eq!(scene["later_field"]["paths"], json!(["", ""]));
    assert!(scene["note"]
        .as_str()
        .unwrap()
        .starts_with("built from <datasets>/demo/"));

    std::fs::write(ds_root.join("rawdata/sub-01/rawimages.json"), b"{broken").unwrap();
    for uri in [
        "/api/v1/datasets/demo/subjects/01",
        "/api/v1/datasets/demo/subjects/01/qc/checkreg",
    ] {
        let (_, bytes) = call(&app, get(uri)).await;
        assert_no_root(uri, &bytes, root.path());
    }
}
