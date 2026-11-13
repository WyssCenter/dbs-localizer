// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests of the run queue, SSE streams, QC gate, locks, cancellation, the static web
//! client and CORS of the `dbsloc-server` router. GPU-free: runs use the fake runner scripts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::stages::Stage;
use dbsloc_server::db::NewJob;
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

fn runner(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    dir.join(if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        format!("{name}.sh")
    })
}

fn config(root: &Path, runner_path: PathBuf) -> Config {
    let mut c = Config::default_for_root(root);
    c.templates_dir = Some(root.join("templates"));
    c.pipeline_runner_bin = Some(runner_path);
    c
}

fn live_app(root: &Path, runner_name: &str) -> (AppState, Router) {
    let state = AppState::new(config(root, runner(runner_name))).unwrap();
    let router = build_router(state.clone());
    (state, router)
}

fn idle_app(root: &Path) -> (AppState, Router) {
    let state = AppState::new_idle(config(root, runner("fake_runner"))).unwrap();
    let router = build_router(state.clone());
    (state, router)
}

fn start_worker(state: &AppState) {
    let weak = Arc::downgrade(&state.inner);
    std::thread::spawn(move || dbsloc_server::runs::worker_loop(weak));
}

async fn call_full(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let (status, _, bytes) = call_full(app, req).await;
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

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("not JSON: {e}: {}", String::from_utf8_lossy(bytes)))
}

async fn poll_until<F: Fn(&Value) -> bool>(app: &Router, uri: &str, done: F) -> Value {
    for _ in 0..200 {
        let (_, bytes) = call(app, get(uri)).await;
        let v = value(&bytes);
        if done(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out polling {uri}");
}

async fn collect_sse(resp: axum::response::Response) -> String {
    assert_eq!(resp.status(), StatusCode::OK);
    let collected = tokio::time::timeout(Duration::from_secs(15), resp.into_body().collect())
        .await
        .expect("the event stream must end")
        .unwrap();
    String::from_utf8_lossy(&collected.to_bytes()).into_owned()
}

async fn sse_text(app: &Router, uri: &str, last_event_id: Option<u64>) -> String {
    let mut req = get(uri);
    if let Some(id) = last_event_id {
        req.headers_mut()
            .insert("last-event-id", id.to_string().parse().unwrap());
    }
    collect_sse(app.clone().oneshot(req).await.unwrap()).await
}

fn subj_dir(root: &Path) -> PathBuf {
    root.join("datasets/demo/derivatives/leaddbs/sub-01")
}

fn write_manifest(root: &Path, slides: Vec<Value>) {
    let dir = subj_dir(root).join("locks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("checkreg_manifest.json"),
        serde_json::to_vec(&json!({ "slides": slides })).unwrap(),
    )
    .unwrap();
}

fn slide(branch: &str, modality: &str, method_log: &Path) -> Value {
    json!({
        "checkreg_png": "",
        "moving": "",
        "fixed": "",
        "branch": branch,
        "modality": modality,
        "method_log": method_log.to_string_lossy(),
    })
}

fn insert_job(
    state: &AppState,
    root: &Path,
    id: &str,
    phase: &str,
    gated: Option<&str>,
    stages: Option<Vec<Stage>>,
) {
    let mut opts = RunOptions::new(root.join("datasets/demo"), "01");
    if let Some(s) = stages {
        opts.stages = s;
    }
    state
        .db()
        .insert_as(
            &NewJob {
                id: id.to_owned(),
                dataset: "demo".to_owned(),
                subject: "01".to_owned(),
                options: serde_json::to_string(&opts).unwrap(),
                priority: 1,
                resume: false,
                gated_stage: gated.map(str::to_owned),
                client: Some("test".to_owned()),
            },
            phase,
        )
        .unwrap();
}

fn approval_uri(slide: usize) -> String {
    format!("/api/v1/datasets/demo/subjects/01/qc/checkreg/{slide}/approval")
}

async fn submit(app: &Router, sid: &str, body: &Value) -> (StatusCode, Value) {
    let (status, bytes) = call(
        app,
        post_json(&format!("/api/v1/datasets/demo/subjects/{sid}/runs"), body),
    )
    .await;
    (status, value(&bytes))
}

#[tokio::test]
async fn cancelling_an_awaiting_qc_job_frees_the_subject() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    insert_job(
        &state,
        root.path(),
        "a",
        "awaiting_qc",
        Some("coregister"),
        None,
    );

    let (status, _) = call(&app, post_json("/api/v1/runs/a/cancel", &json!({}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let st = value(&call(&app, get("/api/v1/runs/a")).await.1);
    assert_eq!(st["phase"], "cancelled");

    let (status, _) = submit(&app, "01", &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_rejected_slide_ends_the_run_and_frees_the_subject() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(
        &state,
        root.path(),
        "a",
        "awaiting_qc",
        Some("coregister"),
        None,
    );

    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": false}))).await;
    assert_eq!(status, StatusCode::OK);
    let body = value(&bytes);
    assert_eq!(body["all_approved"], json!(false));
    assert!(body.get("resumed_run").is_none());

    let st = value(&call(&app, get("/api/v1/runs/a")).await.1);
    assert_eq!(st["phase"], "rejected");
    assert_eq!(st["message"], "qc rejected: coregister");
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    assert_eq!(written["approval"], json!({"CT": 0}));

    let (status, _) = submit(&app, "01", &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_rejected_scalar_slide_keeps_the_approval_at_zero() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("normalization/log/sub-01_desc-normmethod.json");
    write_manifest(
        root.path(),
        vec![slide("norm", "CT", &log), slide("norm", "MR", &log)],
    );
    let (status, _) = call(&app, post_json(&approval_uri(0), &json!({"ok": false}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, bytes) = call(&app, post_json(&approval_uri(1), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value(&bytes)["all_approved"], json!(false));
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    assert_eq!(written["approval"], json!(0));
}

#[tokio::test]
async fn a_scalar_branch_is_approved_only_when_every_slide_is() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("normalization/log/sub-01_desc-normmethod.json");
    write_manifest(
        root.path(),
        vec![slide("norm", "CT", &log), slide("norm", "MR", &log)],
    );
    let (_, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(value(&bytes)["all_approved"], json!(false));
    assert!(!log.exists());
    let (_, bytes) = call(&app, post_json(&approval_uri(1), &json!({"ok": true}))).await;
    assert_eq!(value(&bytes)["all_approved"], json!(true));
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
    assert_eq!(written["approval"], json!(1));
}

#[tokio::test]
async fn approving_a_gate_that_was_the_last_stage_retires_the_job_as_done() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(
        &state,
        root.path(),
        "a",
        "awaiting_qc",
        Some("coregister"),
        Some(vec![Stage::Coregister]),
    );

    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::OK);
    let body = value(&bytes);
    assert_eq!(body["all_approved"], json!(true));
    assert!(body.get("resumed_run").is_none());

    let st = value(&call(&app, get("/api/v1/runs/a")).await.1);
    assert_eq!(st["phase"], "done");
    assert!(state
        .db()
        .active_for_subject("demo", "01")
        .unwrap()
        .is_none());
    let (status, _) = submit(&app, "01", &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_final_approvals_resume_exactly_once() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(
        &state,
        root.path(),
        "a",
        "awaiting_qc",
        Some("coregister"),
        None,
    );

    let (r1, r2) = futures::future::join(
        call(&app, post_json(&approval_uri(0), &json!({"ok": true}))),
        call(&app, post_json(&approval_uri(0), &json!({"ok": true}))),
    )
    .await;
    assert_eq!(r1.0, StatusCode::OK);
    assert_eq!(r2.0, StatusCode::OK);
    let resumed: Vec<Value> = [value(&r1.1), value(&r2.1)]
        .into_iter()
        .filter_map(|v| v.get("resumed_run").cloned())
        .collect();
    assert_eq!(resumed.len(), 1, "exactly one approval resumes the run");

    let orig = state.db().get("a").unwrap().unwrap();
    assert_eq!(orig.phase, "resumed");
    assert_eq!(orig.resumed_by.as_deref(), resumed[0].as_str());
    let st = value(&call(&app, get("/api/v1/runs/a")).await.1);
    assert_eq!(st["resumed_by"], resumed[0]);
}

#[tokio::test]
async fn approvals_outside_the_gate_or_the_subject_directory_are_refused() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    let outside = root.path().join("outside.json");
    write_manifest(
        root.path(),
        vec![slide("coreg", "CT", &log), slide("coreg", "CT", &outside)],
    );

    let (status, bytes) = call(&app, post_json(&approval_uri(1), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(value(&bytes)["code"], "E_BAD_KEY");
    assert!(!outside.exists());

    insert_job(
        &state,
        root.path(),
        "a",
        "awaiting_qc",
        Some("normalize"),
        None,
    );
    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_BAD_RUN");
    assert!(!log.exists(), "a refused approval must not write the log");
    assert_eq!(state.db().get("a").unwrap().unwrap().phase, "awaiting_qc");
}

#[tokio::test]
async fn the_event_stream_of_a_queued_run_follows_it_to_the_end() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let (status, body) = submit(&app, "01", &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["id"].as_str().unwrap().to_owned();
    let st = value(&call(&app, get(&format!("/api/v1/runs/{id}"))).await.1);
    assert_eq!(st["phase"], "queued");

    let resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/runs/{id}/events")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    start_worker(&state);
    let sse = collect_sse(resp).await;
    assert!(sse.contains("stage_started"), "{sse}");
    assert!(sse.contains("pipeline_completed"), "{sse}");
    assert!(sse.contains("id: 0"), "{sse}");
    let end = sse.split("event: end").nth(1).expect("an end event");
    assert!(end.contains("\"phase\":\"done\""), "{sse}");
}

#[tokio::test]
async fn the_event_stream_of_a_finished_run_replays_skips_and_ends() {
    let root = scratch_root();
    let (state, app) = live_app(root.path(), "fake_runner");
    let (_, body) = submit(&app, "01", &json!({})).await;
    let id = body["id"].as_str().unwrap().to_owned();
    poll_until(&app, &format!("/api/v1/runs/{id}"), |v| {
        v["phase"] == "done"
    })
    .await;

    let uri = format!("/api/v1/runs/{id}/events");
    let full = sse_text(&app, &uri, None).await;
    assert!(full.contains("id: 0"), "{full}");
    assert!(full.contains("id: 1"), "{full}");
    assert!(full.contains("stage_started"), "{full}");
    assert!(full.contains("event: end"), "{full}");

    let tail = sse_text(&app, &uri, Some(0)).await;
    assert!(!tail.contains("stage_started"), "{tail}");
    assert!(tail.contains("pipeline_completed"), "{tail}");
    assert!(tail.contains("event: end"), "{tail}");

    let none = sse_text(&app, &uri, Some(1)).await;
    assert!(!none.contains("pipeline_completed"), "{none}");
    assert!(none.contains("event: end"), "{none}");

    let (status, _) = call(&app, get("/api/v1/runs/nope/events")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    for _ in 0..100 {
        if state.runs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(state.runs().is_empty(), "finished runs leave the registry");
}

#[tokio::test]
async fn subject_spellings_share_one_active_run_slot() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let (status, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/runs/active")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value(&bytes), Value::Null);

    let (status, body) = submit(&app, "sub-01", &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = body["id"].as_str().unwrap().to_owned();
    assert_eq!(state.db().get(&id).unwrap().unwrap().subject, "01");

    for spelling in ["01", "%2001", "sub-01"] {
        let (status, body) = submit(&app, spelling, &json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{spelling}");
        assert_eq!(body["code"], "E_RUN_ACTIVE");
    }
    let (status, body) = submit(&app, "99", &json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "E_NO_SUBJ");

    for spelling in ["sub-01", "01"] {
        let (status, bytes) = call(
            &app,
            get(&format!(
                "/api/v1/datasets/demo/subjects/{spelling}/runs/active"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let active = value(&bytes);
        assert_eq!(active["id"], json!(id));
        assert_eq!(active["phase"], "queued");
        assert_eq!(active["subject"], "01");
    }

    let (status, bytes) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/sub-01/locks/preprocess",
            &json!({"approved": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_RUN_ACTIVE");

    let (status, _) = call(
        &app,
        post_json(&format!("/api/v1/runs/{id}/cancel"), &json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let st = value(&call(&app, get(&format!("/api/v1/runs/{id}"))).await.1);
    assert_eq!(st["phase"], "cancelled");
    assert_eq!(st["message"], "cancelled before start");
    let (_, bytes) = call(&app, get("/api/v1/datasets/demo/subjects/01/runs/active")).await;
    assert_eq!(value(&bytes), Value::Null);
    let (status, _) = call(
        &app,
        post_json(
            "/api/v1/datasets/demo/subjects/01/locks/preprocess",
            &json!({"approved": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn client_options_are_validated_and_the_server_forces_the_gate_stop() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());

    for (body, code) in [
        (json!({"norm_isotropic_mm": 0.1}), "E_BAD_REQUEST"),
        (json!({"norm_isotropic_mm": 9.0}), "E_BAD_REQUEST"),
        (json!({"norm_isotropic_mm": -1.0}), "E_BAD_REQUEST"),
        (json!({"norm_method": "fireants"}), "E_NORM_METHOD"),
        (json!({"recon_method": "magic"}), "E_BAD_REQUEST"),
    ] {
        let (status, got) = submit(&app, "01", &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(got["code"], code, "{body}");
    }
    assert!(state
        .db()
        .active_for_subject("demo", "01")
        .unwrap()
        .is_none());

    let method = if cfg!(feature = "pacer") {
        "pacer"
    } else {
        let (status, got) = submit(&app, "01", &json!({"recon_method": "pacer"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(got["code"], "E_RECON_pacer_unavailable");
        "trac_core_native"
    };
    let (status, body) = submit(
        &app,
        "01",
        &json!({"stop_on_error": false, "norm_isotropic_mm": 1.5, "recon_method": method}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = state
        .db()
        .get(body["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let opts = value(job.options.as_bytes());
    assert_eq!(opts["stop_on_error"], json!(true));
    assert_eq!(opts["norm_isotropic_mm"], json!(1.5));
    assert_eq!(opts["recon_method"], method);
    assert_eq!(opts["reg_backend"], "wgpu");
    assert_eq!(opts["headless"], json!(true));
}

#[tokio::test]
async fn a_qc_gated_run_waits_for_approval_and_resumes_after_it() {
    let root = scratch_root();
    let (state, app) = live_app(root.path(), "fake_runner_qc");
    let (_, body) = submit(&app, "01", &json!({})).await;
    let id = body["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/v1/runs/{id}");
    let st = poll_until(&app, &uri, |v| v["phase"] == "awaiting_qc").await;
    assert_eq!(st["gated_stage"], "normalize");
    assert_eq!(
        st["stages_failed"], 0,
        "a QC-pending stage is not a failure"
    );
    assert!(st["message"].as_str().unwrap().contains("qc not approved"));

    let sse = sse_text(&app, &format!("{uri}/events"), None).await;
    assert!(sse.contains("stage_awaiting_qc"), "{sse}");
    let end = sse.split("event: end").nth(1).expect("an end event");
    assert!(end.contains("\"phase\":\"awaiting_qc\""), "{sse}");

    let (status, _) = submit(&app, "01", &json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let log = subj_dir(root.path()).join("normalization/log/sub-01_desc-normmethod.json");
    write_manifest(root.path(), vec![slide("norm", "", &log)]);
    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::OK);
    let approval = value(&bytes);
    assert_eq!(approval["all_approved"], json!(true));
    let resumed = approval["resumed_run"].as_str().expect("a resume run");
    let orig = state.db().get(&id).unwrap().unwrap();
    assert_eq!(orig.phase, "resumed");
    assert_eq!(orig.resumed_by.as_deref(), Some(resumed));
    let sse = sse_text(&app, &format!("{uri}/events"), None).await;
    assert!(sse.contains("\"phase\":\"resumed\""), "{sse}");
}

#[tokio::test]
async fn shutdown_ends_open_event_streams() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let (_, body) = submit(&app, "01", &json!({})).await;
    let id = body["id"].as_str().unwrap();
    let resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/runs/{id}/events")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    state.shutdown();
    let sse = collect_sse(resp).await;
    assert!(!sse.contains("event: end"), "{sse}");
}

#[tokio::test]
async fn a_runner_that_cannot_start_fails_the_run() {
    let root = scratch_root();
    let missing = root.path().join("no-such-runner");
    let state = AppState::new(config(root.path(), missing)).unwrap();
    let app = build_router(state.clone());
    let (_, body) = submit(&app, "01", &json!({})).await;
    let id = body["id"].as_str().unwrap().to_owned();
    let st = poll_until(&app, &format!("/api/v1/runs/{id}"), |v| {
        v["phase"] == "failed"
    })
    .await;
    assert!(st["message"].as_str().unwrap().contains("failed to start"));
    let sse = sse_text(&app, &format!("/api/v1/runs/{id}/events"), None).await;
    assert!(sse.contains("\"phase\":\"failed\""), "{sse}");
}

#[tokio::test]
async fn the_web_client_is_served_with_a_spa_fallback_and_json_api_errors() {
    let root = scratch_root();
    let web = root.path().join("web");
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), b"<h1>Wyss DBS Localizer</h1>").unwrap();
    std::fs::write(web.join("assets/app_bg.wasm"), b"\0asm").unwrap();
    std::fs::write(web.join("assets/app.js"), b"export {}").unwrap();
    std::fs::write(root.path().join("secret.txt"), b"secret").unwrap();
    let mut cfg = config(root.path(), runner("fake_runner"));
    cfg.web_dir = Some(web);
    let state = AppState::new_idle(cfg).unwrap();
    let app = build_router(state);

    let (status, headers, body) = call_full(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(body, b"<h1>Wyss DBS Localizer</h1>");

    let (status, headers, body) = call_full(&app, get("/assets/app_bg.wasm")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/wasm");
    assert_eq!(body, b"\0asm");

    let (_, headers, _) = call_full(&app, get("/assets/app.js")).await;
    assert_eq!(headers["content-type"], "text/javascript");

    let (status, headers, body) = call_full(&app, get("/subjects/01")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(body, b"<h1>Wyss DBS Localizer</h1>");

    let (status, body) = call(&app, get("/api/x")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value(&body)["code"], "E_NOT_FOUND");
    let (status, body) = call(&app, get("/api/v1/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value(&body)["code"], "E_NOT_FOUND");

    let (status, _) = call(&app, get("/assets/missing.js")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, get("/%2e%2e/secret.txt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, get("/assets/..%2f..%2fsecret.txt")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn without_a_web_directory_the_root_is_a_json_404() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());
    let (status, body) = call(&app, get("/")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(value(&body)["code"], "E_NOT_FOUND");
}

#[tokio::test]
async fn cors_answers_preflights_for_configured_origins_only() {
    let root = scratch_root();
    let mut cfg = config(root.path(), runner("fake_runner"));
    cfg.cors_origins = vec!["http://localhost:8081".to_owned()];
    let state = AppState::new_idle(cfg).unwrap();
    let app = build_router(state);

    let preflight = |origin: &str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/datasets")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap()
    };
    let (status, headers, _) = call_full(&app, preflight("http://localhost:8081")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers["access-control-allow-origin"],
        "http://localhost:8081"
    );
    assert_eq!(
        headers["access-control-allow-methods"],
        "GET, POST, PUT, OPTIONS"
    );
    assert_eq!(
        headers["access-control-allow-headers"],
        "content-type, range, last-event-id"
    );
    assert_eq!(headers["vary"], "Origin");

    let (_, headers, _) = call_full(&app, preflight("http://evil.example")).await;
    assert!(headers.get("access-control-allow-origin").is_none());

    let mut req = get("/api/v1/datasets");
    req.headers_mut()
        .insert("origin", "http://localhost:8081".parse().unwrap());
    let (status, headers, _) = call_full(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["access-control-allow-origin"],
        "http://localhost:8081"
    );
    assert_eq!(headers["vary"], "Origin");

    let (status, headers, _) = call_full(&app, get("/api/v1/datasets")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("access-control-allow-origin").is_none());
}

fn with_headers(mut req: Request<Body>, pairs: &[(&'static str, &str)]) -> Request<Body> {
    for (k, v) in pairs {
        req.headers_mut().insert(*k, v.parse().unwrap());
    }
    req
}

async fn phase(app: &Router, id: &str) -> Value {
    value(&call(app, get(&format!("/api/v1/runs/{id}"))).await.1)["phase"].clone()
}

fn post_empty(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn foreign_host_names_are_refused_and_ip_literals_and_localhost_pass() {
    let root = scratch_root();
    let (_state, app) = idle_app(root.path());

    let (status, body) = call(
        &app,
        with_headers(get("/api/v1/datasets"), &[("host", "evil.example:8080")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_HOST");

    let (status, body) = call(
        &app,
        Request::builder()
            .uri("http://evil.example:8080/api/v1/datasets")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_HOST");

    let (status, body) = call(&app, with_headers(get("/"), &[("host", "evil.example")])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_HOST");

    for host in [
        "",
        "user@127.0.0.1:8080",
        "::1",
        "2130706433:8080",
        "127.0.0.1.evil.example",
    ] {
        let (status, body) = call(
            &app,
            with_headers(get("/api/v1/datasets"), &[("host", host)]),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host:?}");
        assert_eq!(value(&body)["code"], "E_HOST", "{host:?}");
    }

    let absolute = |uri: &str, host: &str| {
        with_headers(
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
            &[("host", host)],
        )
    };
    let (status, _) = call(
        &app,
        absolute("http://evil.example/api/v1/datasets", "127.0.0.1:8080"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        absolute("http://127.0.0.1:8080/api/v1/datasets", "evil.example"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_HOST");

    let (status, plain) = call(&app, get("/api/v1/datasets")).await;
    assert_eq!(status, StatusCode::OK);
    for host in [
        "127.0.0.1:8080",
        "localhost:8080",
        "[::1]:8080",
        "LOCALHOST.:8080",
    ] {
        let (status, body) = call(
            &app,
            with_headers(get("/api/v1/datasets"), &[("host", host)]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{host}");
        assert_eq!(body, plain, "{host}");
    }
}

#[tokio::test]
async fn configured_allowed_hosts_pass_case_insensitively() {
    let root = scratch_root();
    let mut cfg = config(root.path(), runner("fake_runner"));
    cfg.allowed_hosts = vec!["dbs.lab.example".to_owned()];
    let app = build_router(AppState::new_idle(cfg).unwrap());

    for host in [
        "dbs.lab.example",
        "DBS.Lab.Example:8080",
        "dbs.lab.example.:443",
    ] {
        let (status, _) = call(
            &app,
            with_headers(get("/api/v1/datasets"), &[("host", host)]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    let (status, _) = call(
        &app,
        with_headers(get("/api/v1/datasets"), &[("host", "other.lab.example")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_site_writes_are_refused_and_same_origin_writes_pass() {
    let root = scratch_root();
    let mut cfg = config(root.path(), runner("fake_runner"));
    cfg.cors_origins = vec!["http://localhost:8081".to_owned()];
    let state = AppState::new_idle(cfg).unwrap();
    let app = build_router(state.clone());
    let waiting = |id: &str| {
        insert_job(
            &state,
            root.path(),
            id,
            "awaiting_qc",
            Some("coregister"),
            None,
        )
    };
    let host = ("host", "127.0.0.1:8080");
    waiting("a");

    let (status, body) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/a/cancel"),
            &[host, ("origin", "http://evil.example")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_ORIGIN");
    assert_eq!(phase(&app, "a").await, "awaiting_qc");

    let (status, body) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/a/cancel"),
            &[host, ("origin", "null")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_ORIGIN");

    let (status, body) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/a/cancel"),
            &[host, ("sec-fetch-site", "cross-site")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_ORIGIN");
    assert_eq!(phase(&app, "a").await, "awaiting_qc");

    let (status, _) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/a/cancel"),
            &[host, ("origin", "http://127.0.0.1:8080")],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(phase(&app, "a").await, "cancelled");

    waiting("b");
    let (status, _) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/b/cancel"),
            &[
                host,
                ("origin", "http://localhost:8081"),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(phase(&app, "b").await, "cancelled");

    waiting("c");
    let (status, _) = call(
        &app,
        with_headers(post_empty("/api/v1/runs/c/cancel"), &[host]),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    waiting("d");
    let (status, _) = call(&app, post_empty("/api/v1/runs/d/cancel")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, headers, _) = call_full(
        &app,
        with_headers(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/runs/a/cancel")
                .body(Body::empty())
                .unwrap(),
            &[
                host,
                ("origin", "http://localhost:8081"),
                ("access-control-request-method", "POST"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers["access-control-allow-origin"],
        "http://localhost:8081"
    );

    let (status, _) = call(
        &app,
        with_headers(
            get("/api/v1/datasets"),
            &[
                host,
                ("origin", "http://evil.example"),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for uri in [
        "/api/v1/datasets/demo/groups",
        "/api/v1/connectomes/x/fetch/cancel",
        "/api/v1/datasets/demo/subjects/01/stimulations/import",
    ] {
        let mut req = with_headers(post_empty(uri), &[host, ("origin", "http://evil.example")]);
        req.headers_mut()
            .insert("content-type", "text/plain".parse().unwrap());
        let (status, body) = call(&app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(value(&body)["code"], "E_ORIGIN", "{uri}");
    }

    let (status, headers, body) = call_full(
        &app,
        with_headers(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/runs/a/cancel")
                .body(Body::empty())
                .unwrap(),
            &[
                ("host", "evil.example:8080"),
                ("origin", "http://localhost:8081"),
                ("access-control-request-method", "POST"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_HOST");
    assert!(headers.get("access-control-allow-origin").is_none());

    waiting("e");
    let (status, body) = call(
        &app,
        with_headers(
            post_empty("/api/v1/runs/e/cancel"),
            &[
                ("host", "localhost:8080"),
                ("origin", "http://localhost:3000"),
            ],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value(&body)["code"], "E_ORIGIN");
    assert_eq!(phase(&app, "e").await, "awaiting_qc");
}

#[tokio::test]
async fn approvals_are_refused_while_the_subject_has_a_running_run() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(&state, root.path(), "a", "running", None, None);

    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_RUN_ACTIVE");
    assert!(!log.exists());
}

#[tokio::test]
async fn approvals_are_refused_while_a_fresh_run_is_queued() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(&state, root.path(), "a", "queued", None, None);

    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value(&bytes)["code"], "E_RUN_ACTIVE");
    assert!(!log.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_right_after_the_gate_waits_for_the_runner_to_exit() {
    let root = scratch_root();
    let (state, app) = idle_app(root.path());
    let log = subj_dir(root.path()).join("coregistration/log/sub-01_desc-coregmethod.json");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, br#"{"method":"ants_multistart","approval":0}"#).unwrap();
    write_manifest(root.path(), vec![slide("coreg", "CT", &log)]);
    insert_job(
        &state,
        root.path(),
        "a",
        "running",
        Some("coreg"),
        Some(vec![Stage::Coregister, Stage::Normalize]),
    );
    let finisher = {
        let state = state.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            state
                .db()
                .finish("a", "awaiting_qc", 1, 0, 0, None)
                .unwrap();
        })
    };

    let (status, bytes) = call(&app, post_json(&approval_uri(0), &json!({"ok": true}))).await;
    finisher.join().unwrap();
    assert_eq!(status, StatusCode::OK, "{}", value(&bytes));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_approvals_of_sibling_slides_both_land() {
    for round in 0..5 {
        let root = scratch_root();
        let (_state, app) = idle_app(root.path());
        let log = subj_dir(root.path()).join("normalization/log/sub-01_desc-normmethod.json");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, br#"{"method":"ants_syn","approval":0}"#).unwrap();
        write_manifest(
            root.path(),
            vec![slide("norm", "T1w", &log), slide("norm", "CT", &log)],
        );
        let (r1, r2) = futures::future::join(
            call(&app, post_json(&approval_uri(0), &json!({"ok": true}))),
            call(&app, post_json(&approval_uri(1), &json!({"ok": true}))),
        )
        .await;
        assert_eq!(r1.0, StatusCode::OK, "round {round}");
        assert_eq!(r2.0, StatusCode::OK, "round {round}");
        let all: Vec<Value> = [value(&r1.1), value(&r2.1)]
            .into_iter()
            .map(|v| v["all_approved"].clone())
            .collect();
        assert!(all.contains(&json!(true)), "round {round}: {all:?}");
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        assert_eq!(written["approval"], json!(1), "round {round}");
        let decisions = log.with_file_name("sub-01_desc-normmethod.json.qc-slides.json");
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&decisions).unwrap()).unwrap();
        assert_eq!(
            stored["slides"],
            json!({"0": true, "1": true}),
            "round {round}"
        );
    }
}
