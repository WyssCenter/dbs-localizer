// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP handlers for the `/api/v1` router.
//!
//! Every handler that touches the filesystem, SQLite or the pipeline library runs its body on a
//! blocking worker (`blocking`), never on a tokio worker thread.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use dbsloc_api::{
    ApprovalRequest, ApprovalResponse, DatasetSummary, GpuInfo, LockRequest, RunStatus,
    SubjectSummary,
};
use dbsloc_pipeline::bids::simplify_verbatim;
use dbsloc_pipeline::checkreg;
use dbsloc_pipeline::locks;
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::reg_backend::cached_probe;
use dbsloc_pipeline::stages::Stage;
use dbsloc_pipeline::subject_summary::subject_summary as build_subject_summary;
use serde_json::{json, Value};

use crate::audit::AuditEntry;
use crate::config::Config;
use crate::error::{ServerErr, E_BAD_KEY, E_BAD_RUN, E_BAD_STAGE, E_NO_MANIFEST, E_RUN_ACTIVE};
use crate::files::{resolve_in_root, serve, valid_dataset_name};
use crate::middleware::ClientAddr;
use crate::runs::{canonical_subject, ResumeOutcome};
use crate::state::AppState;

pub use crate::uploads::{upload_chunk, upload_commit, upload_convert, upload_init};

/// Run a blocking handler body on a blocking worker.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T, ServerErr>
where
    F: FnOnce() -> Result<T, ServerErr> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await?
}

pub(crate) fn require_dataset(state: &AppState, ds: &str) -> Result<PathBuf, ServerErr> {
    if !valid_dataset_name(ds) {
        return Err(ServerErr::bad_request(
            crate::error::E_BAD_KEY,
            "invalid dataset name",
        ));
    }
    let root = state.dataset_root(ds);
    if !root.is_dir() {
        return Err(ServerErr::not_found(format!("no dataset {ds}")));
    }
    Ok(root)
}

/// Whether `key` (a `/`-separated file key under a dataset root) belongs to `sid`: either
/// `rawdata/sub-<sid>/…` or a `derivatives/…` path containing a `sub-<sid>` component.
fn key_belongs_to_subject(key: &str, sid: &str) -> bool {
    let sid = sid.strip_prefix("sub-").unwrap_or(sid);
    let sub = format!("sub-{sid}");
    let comps: Vec<&str> = key.split('/').filter(|c| !c.is_empty()).collect();
    match comps.first().copied() {
        Some("rawdata") => comps.get(1).copied() == Some(sub.as_str()),
        Some("derivatives") => comps.contains(&sub.as_str()),
        _ => false,
    }
}

pub(crate) fn audit(
    state: &AppState,
    client: &str,
    dataset: Option<&str>,
    subject: Option<&str>,
    action: &str,
    detail: Option<String>,
) {
    let _ = crate::audit::append(
        &state.audit_log(),
        &AuditEntry::now(
            client,
            dataset.map(str::to_owned),
            subject.map(str::to_owned),
            action,
            detail,
        ),
    );
}

/// `GET /gpu` — adapter, graphics API and self-test verdict.
pub async fn gpu(State(_state): State<AppState>) -> Result<Json<GpuInfo>, ServerErr> {
    blocking(|| {
        let probe = cached_probe();
        Ok(Json(GpuInfo {
            adapter: probe
                .adapter
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned()),
            backend: probe
                .backend
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned()),
            verdict: probe.verdict.clone(),
        }))
    })
    .await
}

fn is_dataset_root(dir: &Path) -> bool {
    if dir.join("rawdata").is_dir() || dir.join("dataset_description.json").is_file() {
        return true;
    }
    for sub in ["rawdata", "sourcedata", "derivatives"] {
        if let Ok(rd) = std::fs::read_dir(dir.join(sub)) {
            if rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("sub-") && e.path().is_dir()
            }) {
                return true;
            }
        }
    }
    false
}

/// `GET /datasets`
pub async fn list_datasets(
    State(state): State<AppState>,
) -> Result<Json<Vec<DatasetSummary>>, ServerErr> {
    blocking(move || {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(state.config().datasets_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_dir() || !is_dataset_root(&p) {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let subjects = dbsloc_pipeline::bids::list_subject_ids(&p).len() as u32;
                out.push(DatasetSummary { name, subjects });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Json(out))
    })
    .await
}

/// `GET /datasets/{ds}/subjects`
pub async fn list_subjects(
    State(state): State<AppState>,
    AxumPath(ds): AxumPath<String>,
) -> Result<Json<Vec<String>>, ServerErr> {
    blocking(move || {
        let root = require_dataset(&state, &ds)?;
        Ok(Json(dbsloc_pipeline::bids::list_subject_ids(&root)))
    })
    .await
}

/// `GET /datasets/{ds}/subjects/{sid}`
pub async fn subject_summary(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<SubjectSummary>, ServerErr> {
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        let opts = RunOptions::new(&root, &sid);
        let data = build_subject_summary(&root, &sid, &opts)?;
        Ok(Json(crate::dto::subject_summary(&data, &root)))
    })
    .await
}

/// `POST /datasets/{ds}/subjects/{sid}/plan` — read-only plan with per-stage decisions.
pub async fn plan(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        let mut opts = RunOptions::new(&root, &sid);
        if let Some(t) = state.config().template_t1() {
            opts.normalization_template = Some(t);
        }
        let run_plan = dbsloc_pipeline::plan_run(&opts)?;
        let summary = build_subject_summary(&root, &sid, &opts)?;
        let state_of = |stage: &str| {
            summary
                .stages
                .iter()
                .find(|s| s.stage == stage)
                .map(|s| s.state.clone())
                .unwrap_or_else(|| "missing".to_owned())
        };
        let decisions: Vec<Value> = opts
            .stages
            .iter()
            .map(|stage| {
                let name = stage.cli_name();
                let state = state_of(name);
                let (decision, detail) = if state == "approved" && !opts.overwrite_approved {
                    ("skip:approved", None)
                } else if let Some(msg) =
                    dbsloc_pipeline::prereqs::prereq_error(&run_plan.subject, &opts, *stage)
                {
                    ("blocked", Some(msg))
                } else {
                    ("run", None)
                };
                json!({ "stage": name, "state": state, "decision": decision, "detail": detail })
            })
            .collect();
        let mut out = run_plan.to_json(&opts);
        out["decisions"] = Value::Array(decisions);
        Ok(Json(out))
    })
    .await
}

/// `GET /datasets/{ds}/subjects/{sid}/qc/checkreg`
pub async fn qc_manifest(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<dbsloc_api::QcManifest>, ServerErr> {
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        let sub = dbsloc_pipeline::bids::resolve_subject_with(&root, &sid, true)?;
        let path = checkreg::manifest_path(&sub);
        if !path.is_file() {
            return Err(ServerErr::not_found_code(
                E_NO_MANIFEST,
                format!("no checkreg manifest for sub-{sid}"),
            ));
        }
        let text = std::fs::read_to_string(&path)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| ServerErr::internal(format!("checkreg manifest: {e}")))?;
        Ok(Json(crate::dto::qc_manifest(&value, &root)))
    })
    .await
}

/// `p` canonicalised for a containment check even when it does not exist yet: the deepest existing
/// ancestor is canonicalised and the missing tail appended.
fn canonical_allowing_missing(p: &Path) -> Option<PathBuf> {
    let mut tail = Vec::new();
    let mut cur = p.to_path_buf();
    let mut out = loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            break simplify_verbatim(c);
        }
        tail.push(cur.file_name()?.to_owned());
        if !cur.pop() {
            return None;
        }
    };
    for name in tail.iter().rev() {
        out.push(name);
    }
    Some(out)
}

/// The method log of a manifest slide, required to lie under the subject directory.
fn contained_method_log(subj_dir: &Path, method_log: &str) -> Result<PathBuf, ServerErr> {
    let outside = || {
        ServerErr::bad_request(
            E_BAD_KEY,
            "the slide's method log is outside the subject directory",
        )
    };
    let base = std::fs::canonicalize(subj_dir)
        .map(simplify_verbatim)
        .map_err(|e| ServerErr::internal(format!("subject directory: {e}")))?;
    let path = canonical_allowing_missing(Path::new(method_log)).ok_or_else(outside)?;
    if !path.starts_with(&base) {
        return Err(outside());
    }
    Ok(path)
}

const GATE_EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
const GATE_EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(100);

fn run_stopped_for_approval(state: &AppState, ds: &str, sid: &str) -> Result<bool, ServerErr> {
    let deadline = std::time::Instant::now() + GATE_EXIT_WAIT;
    loop {
        let Some(job) = state.db().active_for_subject(ds, sid)? else {
            return Ok(true);
        };
        if job.phase == "queued" {
            return Ok(job.resume);
        }
        if job.phase != "running" {
            return Ok(true);
        }
        if job.gated_stage.is_none() || std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(GATE_EXIT_POLL);
    }
}

fn approve_slide(
    state: &AppState,
    ds: &str,
    sid: &str,
    slide: &str,
    ok: bool,
    client: &str,
) -> Result<ApprovalResponse, ServerErr> {
    let sid = canonical_subject(sid)?;
    let root = require_dataset(state, ds)?;
    let lock = state.subject_lock(ds, &sid);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    if !run_stopped_for_approval(state, ds, &sid)? {
        return Err(ServerErr::conflict(
            E_RUN_ACTIVE,
            format!(
                "sub-{sid} has a queued or running run; approve its registrations after it stops"
            ),
        ));
    }
    let sub = dbsloc_pipeline::bids::resolve_subject_with(&root, &sid, true)?;
    let path = checkreg::manifest_path(&sub);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| ServerErr::not_found_code(E_NO_MANIFEST, "no checkreg manifest"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| ServerErr::internal(format!("checkreg manifest: {e}")))?;
    let index: usize = slide
        .parse()
        .map_err(|_| ServerErr::bad_request(E_NO_MANIFEST, format!("bad slide {slide}")))?;
    let row = value
        .get("slides")
        .and_then(Value::as_array)
        .and_then(|a| a.get(index))
        .ok_or_else(|| ServerErr::not_found(format!("no slide {index}")))?;
    let method_log = row
        .get("method_log")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ServerErr::bad_request(E_NO_MANIFEST, "slide has no method_log"))?;
    let log_path = contained_method_log(&sub.subj_dir, method_log)?;
    let branch = row
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let modality = row
        .get("modality")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let awaiting = state.db().awaiting_qc_for_subject(ds, &sid)?;
    if let Some(gated) = awaiting
        .as_ref()
        .and_then(|j| j.gated_stage.as_deref())
        .filter(|g| !g.is_empty())
    {
        if !crate::runs::branch_matches_gate(branch, gated) {
            return Err(ServerErr::conflict(
                E_BAD_RUN,
                format!(
                    "slide {index} belongs to the {branch:?} gate, but the run is waiting at \
                     the {gated:?} gate"
                ),
            ));
        }
    }

    if is_scalar_branch(branch) {
        let siblings = sibling_slides(&value, method_log, branch);
        record_scalar_decision(&log_path, branch, index, ok, &siblings)?;
    } else {
        checkreg::apply_approval(&log_path, branch, modality, ok)?;
    }
    audit(
        state,
        client,
        Some(ds),
        Some(&sid),
        "qc_approval",
        Some(format!("slide={index} ok={ok}")),
    );

    let all_approved = all_slides_approved(&value);
    let mut response = ApprovalResponse {
        all_approved,
        resumed_run: None,
    };
    let Some(job) = awaiting else {
        return Ok(response);
    };
    if !ok {
        crate::runs::reject_awaiting(state, &job)?;
    } else if all_approved {
        if let ResumeOutcome::Resumed(id) =
            crate::runs::submit_resume(state, ds, &sid, &job, client)?
        {
            response.resumed_run = Some(id);
        }
    }
    Ok(response)
}

fn is_scalar_branch(branch: &str) -> bool {
    matches!(
        branch.to_lowercase().as_str(),
        "norm" | "normalization" | "brainshift"
    )
}

fn sibling_slides(manifest: &Value, method_log: &str, branch: &str) -> Vec<usize> {
    let want = branch.to_lowercase();
    manifest
        .get("slides")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .enumerate()
                .filter(|(_, r)| {
                    r.get("method_log").and_then(Value::as_str) == Some(method_log)
                        && r.get("branch")
                            .and_then(Value::as_str)
                            .is_some_and(|b| b.to_lowercase() == want)
                })
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

fn decisions_path(log_path: &Path) -> PathBuf {
    let name = log_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    log_path.with_file_name(format!("{name}.qc-slides.json"))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SlideDecisions {
    log_sha256: Option<String>,
    slides: std::collections::BTreeMap<String, bool>,
}

fn log_digest(log_path: &Path) -> Option<String> {
    dbsloc_pipeline::sha256::file_sha256_hex(log_path).ok()
}

fn read_decisions(
    path: &Path,
    log_sha256: Option<&str>,
) -> std::collections::BTreeMap<String, bool> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<SlideDecisions>(&t).ok())
        .filter(|d| d.log_sha256.as_deref() == log_sha256)
        .map(|d| d.slides)
        .unwrap_or_default()
}

fn write_decisions(path: &Path, decisions: &SlideDecisions) -> Result<(), ServerErr> {
    let text = serde_json::to_string(decisions)
        .map_err(|e| ServerErr::internal(format!("qc decisions: {e}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".partial-{name}-{}", crate::runs::new_id()));
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(|e| ServerErr::internal(format!("qc decisions: {e}")))
}

fn record_scalar_decision(
    log_path: &Path,
    branch: &str,
    index: usize,
    ok: bool,
    siblings: &[usize],
) -> Result<(), ServerErr> {
    let path = decisions_path(log_path);
    let before = log_digest(log_path);
    let mut slides = read_decisions(&path, before.as_deref());
    slides.insert(index.to_string(), ok);
    let decided: Vec<bool> = siblings
        .iter()
        .filter_map(|i| slides.get(&i.to_string()).copied())
        .collect();
    let decisions: Vec<(Option<String>, bool)> = if decided.contains(&false) {
        vec![(None, false)]
    } else if decided.len() == siblings.len() {
        decided.into_iter().map(|d| (None, d)).collect()
    } else {
        Vec::new()
    };
    checkreg::apply_decisions(log_path, branch, &decisions).map_err(ServerErr::internal)?;
    let log_sha256 = if decisions.is_empty() {
        before
    } else {
        log_digest(log_path)
    };
    write_decisions(&path, &SlideDecisions { log_sha256, slides })
}

/// `POST /datasets/{ds}/subjects/{sid}/qc/checkreg/{slide}/approval`
pub async fn qc_approve(
    State(state): State<AppState>,
    AxumPath((ds, sid, slide)): AxumPath<(String, String, String)>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<ApprovalRequest>,
) -> Result<Json<ApprovalResponse>, ServerErr> {
    blocking(move || approve_slide(&state, &ds, &sid, &slide, req.ok, client.as_str()))
        .await
        .map(Json)
}

fn all_slides_approved(manifest: &Value) -> bool {
    let Some(slides) = manifest.get("slides").and_then(Value::as_array) else {
        return false;
    };
    !slides.is_empty()
        && slides.iter().all(|row| {
            let branch = row
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let modality = row
                .get("modality")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            row.get("method_log")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .is_some_and(|p| checkreg::slide_approved(Path::new(p), branch, modality))
        })
}

/// `POST /datasets/{ds}/subjects/{sid}/locks/{stage}`
pub async fn lock_approve(
    State(state): State<AppState>,
    AxumPath((ds, sid, stage)): AxumPath<(String, String, String)>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<LockRequest>,
) -> Result<StatusCode, ServerErr> {
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        let stage = Stage::parse(&stage)
            .ok_or_else(|| ServerErr::bad_request(E_BAD_STAGE, format!("unknown stage {stage}")))?;
        if state.db().active_for_subject(&ds, &sid)?.is_some() {
            return Err(ServerErr::conflict(
                E_RUN_ACTIVE,
                format!("sub-{sid} has a queued, running or awaiting-QC run; locks are frozen"),
            ));
        }
        let sub = dbsloc_pipeline::bids::resolve_subject_with(&root, &sid, true)?;
        let path = locks::stage_approval_path(&sub.locks_dir(), stage.cli_name());
        locks::write_approval(&path, req.approved, None, &[], None)?;
        audit(
            &state,
            client.as_str(),
            Some(&ds),
            Some(&sid),
            "lock_change",
            Some(format!(
                "stage={} approved={}",
                stage.cli_name(),
                req.approved
            )),
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// `GET /datasets/{ds}/subjects/{sid}/viz/scene`
pub async fn viz_scene(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<Value>, ServerErr> {
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        let sub = dbsloc_pipeline::bids::resolve_subject_with(&root, &sid, true)?;
        let candidates = [
            sub.recon_dir.join("viz").join("scene_manifest.json"),
            sub.subj_dir.join("viz").join("scene_manifest.json"),
        ];
        let path = candidates.iter().find(|p| p.is_file());
        let Some(path) = path else {
            return Err(ServerErr::not_found("no scene manifest"));
        };
        let text = std::fs::read_to_string(path)?;
        let mut value: Value = serde_json::from_str(&text)
            .map_err(|e| ServerErr::internal(format!("scene manifest: {e}")))?;
        rewrite_scene_paths(&mut value, &root, state.config());
        Ok(Json(value))
    })
    .await
}

/// Rewrite every absolute path string of a scene manifest to a dataset/template/atlas file key,
/// or to an empty string when it lies outside those roots.
fn rewrite_scene_paths(value: &mut Value, root: &Path, config: &Config) {
    match value {
        Value::String(s) if crate::sanitize::looks_absolute(s) => {
            *s = manifest_key(root, Path::new(s.as_str()), config).unwrap_or_default();
        }
        Value::Array(items) => {
            for item in items {
                rewrite_scene_paths(item, root, config);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                rewrite_scene_paths(item, root, config);
            }
        }
        _ => {}
    }
}

fn manifest_key(root: &Path, path: &Path, config: &Config) -> Option<String> {
    if let Some(k) = crate::files::file_key(root, path) {
        return Some(k);
    }
    if let Some(dir) = config.templates_dir.as_ref() {
        if let Some(k) = crate::files::file_key(dir, path) {
            return Some(format!("templates/{k}"));
        }
    }
    if let Some(dir) = config.atlas_dir.as_ref() {
        if let Some(k) = crate::files::file_key(dir, path) {
            return Some(format!("atlas/{k}"));
        }
    }
    None
}

fn range_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// `GET /datasets/{ds}/subjects/{sid}/files/{*key}`
pub async fn get_dataset_file(
    State(state): State<AppState>,
    AxumPath((ds, sid, key)): AxumPath<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let range = range_of(&headers);
    blocking(move || {
        let sid = canonical_subject(&sid)?;
        let root = require_dataset(&state, &ds)?;
        if !key_belongs_to_subject(&key, &sid) {
            return Err(ServerErr::bad_request(
                E_BAD_KEY,
                format!("file key is not under sub-{sid}"),
            ));
        }
        if let Some(reason) = crate::files::withheld_reason(&key) {
            return Err(ServerErr::new(StatusCode::FORBIDDEN, E_BAD_KEY, reason));
        }
        let path = resolve_in_root(&root, &key)?;
        serve(&path, range.as_deref())
    })
    .await
}

/// `GET /templates/{*key}`
pub async fn get_template_file(
    State(state): State<AppState>,
    AxumPath(key): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let range = range_of(&headers);
    blocking(move || {
        let dir = state
            .config()
            .templates_dir
            .as_ref()
            .ok_or_else(|| ServerErr::not_found("no templates configured"))?;
        let path = resolve_in_root(dir, &key)?;
        serve(&path, range.as_deref())
    })
    .await
}

/// `GET /atlas/{*key}` — the configured atlas directory (scene/atlas meshes; `specs/04` §4.1).
pub async fn get_atlas_file(
    State(state): State<AppState>,
    AxumPath(key): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let range = range_of(&headers);
    blocking(move || {
        let dir = state
            .config()
            .atlas_dir
            .as_ref()
            .ok_or_else(|| ServerErr::not_found("no atlas configured"))?;
        let path = resolve_in_root(dir, &key)?;
        serve(&path, range.as_deref())
    })
    .await
}

/// `POST /datasets/{ds}/subjects/{sid}/runs`
pub async fn submit_run(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
    Extension(client): Extension<ClientAddr>,
    Json(req): Json<crate::runs::RunRequestExt>,
) -> Result<Response, ServerErr> {
    blocking(move || {
        require_dataset(&state, &ds)?;
        let id = crate::runs::submit_run_ext(&state, &ds, &sid, &req, client.as_str())?;
        Ok((StatusCode::ACCEPTED, Json(json!({ "id": id }))).into_response())
    })
    .await
}

/// `GET /datasets/{ds}/subjects/{sid}/runs/active` — the subject's queued, running or
/// awaiting-QC run, or `null`.
pub async fn active_run(
    State(state): State<AppState>,
    AxumPath((ds, sid)): AxumPath<(String, String)>,
) -> Result<Json<Option<RunStatus>>, ServerErr> {
    blocking(move || {
        require_dataset(&state, &ds)?;
        Ok(Json(crate::runs::active_status(&state, &ds, &sid)?))
    })
    .await
}

/// `GET /runs/{id}`
pub async fn run_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<RunStatus>, ServerErr> {
    blocking(move || Ok(Json(crate::runs::status(&state, &id)?))).await
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct EventsQuery {
    last_event_id: Option<u64>,
}

pub(crate) fn last_event_id(headers: &HeaderMap, query: &EventsQuery) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .or(query.last_event_id)
}

/// `GET /runs/{id}/events`
pub async fn run_events(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Response, ServerErr> {
    let last = last_event_id(&headers, &query);
    let stream = blocking(move || crate::runs::event_stream(&state, &id, last)).await?;
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// `POST /runs/{id}/cancel`
pub async fn cancel_run(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Extension(client): Extension<ClientAddr>,
) -> Result<StatusCode, ServerErr> {
    blocking(move || {
        crate::runs::cancel(&state, &id)?;
        audit(
            &state,
            client.as_str(),
            None,
            None,
            "run_cancel",
            Some(format!("id={id}")),
        );
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_names_reject_dot_dotdot_and_separators() {
        for bad in [
            "", ".", "..", "a/b", "a\\b", "/abs", "../x", "a/../b", "sub/..",
        ] {
            assert!(!valid_dataset_name(bad), "{bad} was accepted");
        }
        for good in ["demo", "sub-01", "cohort_2024", "data.set"] {
            assert!(valid_dataset_name(good), "{good} was rejected");
        }
    }

    #[test]
    fn file_keys_are_allowlisted_to_the_subject() {
        assert!(key_belongs_to_subject(
            "rawdata/sub-01/ses-preop/anat/sub-01_ses-preop_T1w.nii.gz",
            "01"
        ));
        assert!(!key_belongs_to_subject(
            "rawdata/sub-02/anat/x.nii.gz",
            "01"
        ));
        assert!(!key_belongs_to_subject("rawdata/other/sub-01/x", "01"));
        assert!(key_belongs_to_subject(
            "derivatives/leaddbs/sub-01/locks/checkreg_manifest.json",
            "01"
        ));
        assert!(!key_belongs_to_subject(
            "derivatives/leaddbs/sub-02/locks/x.json",
            "01"
        ));
        assert!(!key_belongs_to_subject("runs/abc.ndjson", "01"));
        assert!(!key_belongs_to_subject("sourcedata/sub-01/x", "01"));
        assert!(!key_belongs_to_subject("", "01"));
        assert!(key_belongs_to_subject(
            "rawdata/sub-01/anat/x.nii.gz",
            "sub-01"
        ));
    }

    #[test]
    fn scene_paths_outside_the_root_become_empty_not_absolute() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::default_for_root(root.path());
        let subj = root.path().join("derivatives/leaddbs/sub-01");
        std::fs::create_dir_all(&subj).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut value = json!({
            "template_nii": "/etc/passwd",
            "reco_json": "relative/thing.json",
            "subj_dir": subj.to_string_lossy(),
            "space": "MNI152NLin2009bAsym",
            "future_field": {"nested": [outside.path().to_string_lossy(), "C:/x/y"]},
        });
        rewrite_scene_paths(&mut value, root.path(), &config);
        assert_eq!(value["template_nii"], "");
        assert_eq!(value["reco_json"], "relative/thing.json");
        assert_eq!(value["subj_dir"], "derivatives/leaddbs/sub-01");
        assert_eq!(value["space"], "MNI152NLin2009bAsym");
        assert_eq!(value["future_field"]["nested"], json!(["", ""]));
    }

    #[test]
    fn slide_decisions_reset_when_the_method_log_changes() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("log/norm.json");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, br#"{"method":"ants_syn","approval":0}"#).unwrap();
        let siblings = [0, 1];
        record_scalar_decision(&log, "norm", 0, true, &siblings).unwrap();
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(decisions_path(&log)).unwrap()).unwrap();
        assert_eq!(stored["slides"], json!({"0": true}));
        assert_eq!(stored["log_sha256"].as_str(), log_digest(&log).as_deref());
        record_scalar_decision(&log, "norm", 1, true, &siblings).unwrap();
        assert!(checkreg::slide_approved(&log, "norm", None));
        record_scalar_decision(&log, "norm", 0, true, &siblings).unwrap();
        assert!(checkreg::slide_approved(&log, "norm", None));

        std::fs::write(&log, br#"{"method":"ants_syn","approval":0,"run":2}"#).unwrap();
        record_scalar_decision(&log, "norm", 1, true, &siblings).unwrap();
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(decisions_path(&log)).unwrap()).unwrap();
        assert_eq!(stored["slides"], json!({"1": true}));
        assert!(!checkreg::slide_approved(&log, "norm", None));

        std::fs::write(decisions_path(&log), br#"{"0": true}"#).unwrap();
        record_scalar_decision(&log, "norm", 1, true, &siblings).unwrap();
        assert!(!checkreg::slide_approved(&log, "norm", None));
    }

    #[test]
    fn method_logs_must_stay_under_the_subject_directory() {
        let d = tempfile::tempdir().unwrap();
        let subj = d.path().join("sub-01");
        std::fs::create_dir_all(subj.join("coregistration/log")).unwrap();
        let inside_new = subj.join("coregistration/log/new.json");
        assert!(contained_method_log(&subj, &inside_new.to_string_lossy()).is_ok());
        let nested_missing = subj.join("normalization/log/new.json");
        assert!(contained_method_log(&subj, &nested_missing.to_string_lossy()).is_ok());
        let outside = d.path().join("other/log.json");
        assert!(contained_method_log(&subj, &outside.to_string_lossy()).is_err());
        let traversal = subj.join("../escape.json");
        assert!(contained_method_log(&subj, &traversal.to_string_lossy()).is_err());
        assert!(contained_method_log(&subj, "").is_err());
    }

    #[test]
    fn last_event_ids_parse_or_are_ignored() {
        let mut h = HeaderMap::new();
        let none = EventsQuery::default();
        let q = EventsQuery {
            last_event_id: Some(3),
        };
        assert_eq!(last_event_id(&h, &none), None);
        assert_eq!(last_event_id(&h, &q), Some(3));
        h.insert("last-event-id", " 7 ".parse().unwrap());
        assert_eq!(last_event_id(&h, &q), Some(7));
        h.insert("last-event-id", "x".parse().unwrap());
        assert_eq!(last_event_id(&h, &none), None);
    }
}
