// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Async HTTP client for the Wyss DBS Localizer server API (`docs/SERVER_CLIENT_PLAN.md` §4).
//!
//! Every call is fire-and-forget: [`ehttp::fetch`] runs the request (the browser `fetch` API on
//! `wasm32`) and hands the decoded result back through an [`mpsc`](std::sync::mpsc) channel the
//! app drains each frame, waking the UI with a repaint request. No `std::fs`, no blocking. Paths
//! are opaque file keys, never filesystem paths, exactly as the DTOs in [`dbsloc_api`] define.

use std::sync::mpsc::Sender;

use eframe::egui;
use serde::de::DeserializeOwned;
use serde::Serialize;

use dbsloc_api::{
    ApiError, ApprovalRequest, ApprovalResponse, ConvertedSeries, DatasetSummary, GpuInfo,
    QcManifest, RunEvent, RunRequest, RunStatus, SubjectSummary, UploadCommitRequest,
    UploadInitRequest, UploadInitResponse,
};

use crate::logic::percent_encode_segment;

/// One decoded response (or a run event) delivered to the app.
///
/// Replies that belong to a subject, run or upload carry its identity so the app can drop a
/// reply that arrives after the user moved on.
pub enum ApiMsg {
    Gpu(Result<GpuInfo, ApiError>),
    Datasets(Result<Vec<DatasetSummary>, ApiError>),
    Subjects {
        ds: String,
        result: Result<Vec<String>, ApiError>,
    },
    Subject {
        ds: String,
        sid: String,
        open: bool,
        result: Result<SubjectSummary, ApiError>,
    },
    Plan {
        ds: String,
        sid: String,
        result: Result<serde_json::Value, ApiError>,
    },
    RunCreated {
        ds: String,
        sid: String,
        result: Result<String, ApiError>,
    },
    RunStatus {
        id: String,
        result: Result<RunStatus, ApiError>,
    },
    ActiveRun {
        ds: String,
        sid: String,
        result: Result<Option<RunStatus>, ApiError>,
    },
    Cancel {
        id: String,
        result: Result<(), ApiError>,
    },
    Qc {
        ds: String,
        sid: String,
        result: Result<QcManifest, ApiError>,
    },
    Scene {
        ds: String,
        sid: String,
        result: Result<serde_json::Value, ApiError>,
    },
    Approval {
        ds: String,
        sid: String,
        slide: u32,
        result: Result<ApprovalResponse, ApiError>,
    },
    UploadInit {
        generation: u64,
        result: Result<UploadInitResponse, ApiError>,
    },
    UploadChunk {
        generation: u64,
        file: String,
        chunk: u64,
        len: u64,
        result: Result<(), ApiError>,
    },
    UploadConvert {
        generation: u64,
        result: Result<Vec<ConvertedSeries>, ApiError>,
    },
    UploadCommit {
        generation: u64,
        result: Result<(), ApiError>,
    },
    HashProgress {
        generation: u64,
        done: u64,
    },
    HashDone {
        generation: u64,
        result: Result<Vec<String>, String>,
    },
    File {
        ds: String,
        sid: String,
        key: String,
        result: Result<Vec<u8>, ApiError>,
    },
    RunEvent {
        run_id: String,
        id: Option<u64>,
        event: RunEvent,
    },
    RunEnd {
        run_id: String,
        status: RunStatus,
    },
    SseClosed {
        run_id: String,
    },
}

/// Minimal HTTP client over one server base URL.
#[derive(Clone)]
pub struct ApiClient {
    base: String,
    tx: Sender<ApiMsg>,
    ctx: egui::Context,
}

impl ApiClient {
    pub fn new(base: impl Into<String>, tx: Sender<ApiMsg>, ctx: egui::Context) -> Self {
        Self {
            base: base.into(),
            tx,
            ctx,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    /// Deliver a message to the app and wake the UI.
    pub fn emit(&self, msg: ApiMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    /// Fire one request; `map` decodes the raw response into an [`ApiMsg`].
    fn send<F>(&self, request: ehttp::Request, map: F)
    where
        F: FnOnce(Result<ehttp::Response, String>) -> ApiMsg + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        ehttp::fetch(request, move |res| {
            let _ = tx.send(map(res));
            ctx.request_repaint();
        });
    }

    /// `GET /api/v1/gpu` — adapter, backend and self-test verdict.
    pub fn get_gpu(&self) {
        self.send(get(&self.url("/api/v1/gpu")), |r| {
            ApiMsg::Gpu(decode_json::<GpuInfo>(r))
        });
    }

    /// `GET /api/v1/datasets`.
    pub fn get_datasets(&self) {
        self.send(get(&self.url("/api/v1/datasets")), |r| {
            ApiMsg::Datasets(decode_json::<Vec<DatasetSummary>>(r))
        });
    }

    /// `GET /api/v1/datasets/{ds}/subjects`.
    pub fn get_subjects(&self, ds: &str) {
        let ds = ds.to_owned();
        self.send(
            get(&self.url(&format!("/api/v1/datasets/{ds}/subjects"))),
            move |r| ApiMsg::Subjects {
                ds,
                result: decode_json::<Vec<String>>(r),
            },
        );
    }

    /// `GET /api/v1/datasets/{ds}/subjects/{sid}`; `open` marks the first load of a subject.
    pub fn get_subject(&self, ds: &str, sid: &str, open: bool) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(get(&self.url(&path)), move |r| ApiMsg::Subject {
            ds,
            sid,
            open,
            result: decode_json::<SubjectSummary>(r),
        });
    }

    /// `POST /api/v1/datasets/{ds}/subjects/{sid}/plan`.
    ///
    /// The plan DTO is not part of `dbsloc-api` yet, so the raw JSON is surfaced.
    pub fn post_plan(&self, ds: &str, sid: &str) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/plan");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(json(&self.url(&path), &Empty {}), move |r| ApiMsg::Plan {
            ds,
            sid,
            result: decode_json::<serde_json::Value>(r),
        });
    }

    /// `GET /api/v1/runs/{id}`.
    pub fn get_run(&self, id: &str) {
        let id = id.to_owned();
        self.send(get(&self.url(&format!("/api/v1/runs/{id}"))), move |r| {
            ApiMsg::RunStatus {
                id,
                result: decode_json::<RunStatus>(r),
            }
        });
    }

    /// `GET /api/v1/datasets/{ds}/subjects/{sid}/runs/active` — the queued, running or
    /// awaiting-QC run of the subject, or `null`.
    pub fn get_active_run(&self, ds: &str, sid: &str) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/runs/active");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(get(&self.url(&path)), move |r| ApiMsg::ActiveRun {
            ds,
            sid,
            result: decode_json::<Option<RunStatus>>(r),
        });
    }

    /// `GET /api/v1/datasets/{ds}/subjects/{sid}/qc/checkreg`.
    pub fn get_qc(&self, ds: &str, sid: &str) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/qc/checkreg");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(get(&self.url(&path)), move |r| ApiMsg::Qc {
            ds,
            sid,
            result: decode_json::<QcManifest>(r),
        });
    }

    /// `GET /api/v1/datasets/{ds}/subjects/{sid}/viz/scene` — the scene manifest with file keys.
    pub fn get_scene(&self, ds: &str, sid: &str) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/viz/scene");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(get(&self.url(&path)), move |r| ApiMsg::Scene {
            ds,
            sid,
            result: decode_json::<serde_json::Value>(r),
        });
    }

    /// Fetch a server file key (PNG, NIfTI, JSON) from the endpoint [`file_endpoint`] selects.
    pub fn get_file(&self, ds: &str, sid: &str, key: &str) {
        let key = key.trim_start_matches('/').to_owned();
        let url = file_endpoint(&self.base, ds, sid, &key);
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(get(&url), move |r| ApiMsg::File {
            ds,
            sid,
            key,
            result: decode_bytes(r),
        });
    }

    /// `POST /api/v1/datasets/{ds}/subjects/{sid}/runs` — submit a run; returns the run id.
    pub fn post_run(&self, ds: &str, sid: &str, req: &RunRequest) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/runs");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(json(&self.url(&path), req), move |r| ApiMsg::RunCreated {
            ds,
            sid,
            result: decode_run_id(r),
        });
    }

    /// `POST /api/v1/runs/{id}/cancel`.
    pub fn cancel_run(&self, id: &str) {
        let path = format!("/api/v1/runs/{id}/cancel");
        let id = id.to_owned();
        self.send(json(&self.url(&path), &Empty {}), move |r| ApiMsg::Cancel {
            id,
            result: decode_unit(r),
        });
    }

    /// `POST .../qc/checkreg/{slide}/approval`.
    pub fn post_approval(&self, ds: &str, sid: &str, slide: u32, ok: bool) {
        let path = format!("/api/v1/datasets/{ds}/subjects/{sid}/qc/checkreg/{slide}/approval");
        let (ds, sid) = (ds.to_owned(), sid.to_owned());
        self.send(json(&self.url(&path), &ApprovalRequest { ok }), move |r| {
            ApiMsg::Approval {
                ds,
                sid,
                slide,
                result: decode_approval(r),
            }
        });
    }

    /// `POST /api/v1/datasets/{ds}/uploads` — announce files, get an upload id.
    pub fn init_upload(&self, generation: u64, ds: &str, req: &UploadInitRequest) {
        let path = format!("/api/v1/datasets/{ds}/uploads");
        self.send(json(&self.url(&path), req), move |r| ApiMsg::UploadInit {
            generation,
            result: decode_json::<UploadInitResponse>(r),
        });
    }

    /// `PUT /api/v1/uploads/{id}/{file}/{chunk}` — one raw chunk.
    pub fn put_chunk(
        &self,
        generation: u64,
        upload_id: &str,
        file: &str,
        chunk: u64,
        bytes: Vec<u8>,
    ) {
        let path = format!(
            "/api/v1/uploads/{upload_id}/{}/{chunk}",
            percent_encode_segment(file)
        );
        let file = file.to_owned();
        let len = bytes.len() as u64;
        let request = ehttp::Request::put(self.url(&path), bytes);
        self.send(request, move |r| ApiMsg::UploadChunk {
            generation,
            file,
            chunk,
            len,
            result: decode_unit(r),
        });
    }

    /// `POST /api/v1/uploads/{id}/convert` — run `dcm2niix` on an uploaded DICOM folder.
    pub fn convert_upload(&self, generation: u64, upload_id: &str) {
        let path = format!("/api/v1/uploads/{upload_id}/convert");
        self.send(json(&self.url(&path), &Empty {}), move |r| {
            ApiMsg::UploadConvert {
                generation,
                result: decode_json::<Vec<ConvertedSeries>>(r),
            }
        });
    }

    /// `POST /api/v1/uploads/{id}/commit`.
    pub fn commit_upload(&self, generation: u64, upload_id: &str, req: &UploadCommitRequest) {
        let path = format!("/api/v1/uploads/{upload_id}/commit");
        self.send(json(&self.url(&path), req), move |r| ApiMsg::UploadCommit {
            generation,
            result: decode_unit(r),
        });
    }
}

/// An empty JSON body for endpoints that take no input.
#[derive(Serialize)]
struct Empty {}

fn get(url: &str) -> ehttp::Request {
    ehttp::Request::get(url)
}

fn json(url: &str, body: &impl Serialize) -> ehttp::Request {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let mut request = ehttp::Request::post(url, bytes);
    request.headers.insert("Content-Type", "application/json");
    request
}

/// The server endpoint that serves a scene-manifest file key.
///
/// A key is either a **dataset** file (`<root-relative>`), a **template** file, or an **atlas**
/// file. The server tags paths under its templates / atlas directories with a `templates/` /
/// `atlas/` prefix when it rewrites a scene manifest (`crates/dbsloc-server/src/routes.rs`
/// `rewrite_scene_paths`); those go to the dedicated `/api/v1/templates/{key}` and
/// `/api/v1/atlas/{key}` routes. Every other key goes to the dataset file route.
pub(crate) fn file_endpoint(base: &str, ds: &str, sid: &str, key: &str) -> String {
    let base = base.trim_end_matches('/');
    let key = key.trim_start_matches('/');
    if let Some(rel) = key.strip_prefix("templates/") {
        format!("{base}/api/v1/templates/{rel}")
    } else if let Some(rel) = key.strip_prefix("atlas/") {
        format!("{base}/api/v1/atlas/{rel}")
    } else {
        format!("{base}/api/v1/datasets/{ds}/subjects/{sid}/files/{key}")
    }
}

/// Decode a 2xx JSON body, or the typed `{code, message}` error body otherwise.
fn decode_json<T: DeserializeOwned>(res: Result<ehttp::Response, String>) -> Result<T, ApiError> {
    match res {
        Ok(r) if r.ok => {
            serde_json::from_slice(&r.bytes).map_err(|e| ApiError::new("E_JSON", e.to_string()))
        }
        Ok(r) => Err(decode_error(&r)),
        Err(e) => Err(ApiError::new("E_HTTP", e)),
    }
}

/// Decode a 2xx with no body into `()`, or the typed error otherwise.
fn decode_unit(res: Result<ehttp::Response, String>) -> Result<(), ApiError> {
    match res {
        Ok(r) if r.ok => Ok(()),
        Ok(r) => Err(decode_error(&r)),
        Err(e) => Err(ApiError::new("E_HTTP", e)),
    }
}

/// Decode the approval response; an empty 2xx body counts as the default (nothing resumed).
fn decode_approval(res: Result<ehttp::Response, String>) -> Result<ApprovalResponse, ApiError> {
    match res {
        Ok(r) if r.ok && r.bytes.iter().all(u8::is_ascii_whitespace) => {
            Ok(ApprovalResponse::default())
        }
        other => decode_json::<ApprovalResponse>(other),
    }
}

/// Return the raw response bytes (a file), or the typed error otherwise.
fn decode_bytes(res: Result<ehttp::Response, String>) -> Result<Vec<u8>, ApiError> {
    match res {
        Ok(r) if r.ok => Ok(r.bytes),
        Ok(r) => Err(decode_error(&r)),
        Err(e) => Err(ApiError::new("E_HTTP", e)),
    }
}

/// The run id from a create-run response: a bare JSON string or an object with an `id` field.
fn decode_run_id(res: Result<ehttp::Response, String>) -> Result<String, ApiError> {
    let r = match res {
        Ok(r) if r.ok => r,
        Ok(r) => return Err(decode_error(&r)),
        Err(e) => return Err(ApiError::new("E_HTTP", e)),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&r.bytes).map_err(|e| ApiError::new("E_JSON", e.to_string()))?;
    match &value {
        serde_json::Value::String(s) => Ok(s.clone()),
        other => other
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::new("E_JSON", "run create response carries no id".to_owned())),
    }
}

/// Prefer the pipeline's typed `{code, message}`; fall back to the HTTP status line.
fn decode_error(r: &ehttp::Response) -> ApiError {
    serde_json::from_slice::<ApiError>(&r.bytes).unwrap_or_else(|_| {
        ApiError::new(
            format!("E_HTTP_{}", r.status),
            if r.status_text.is_empty() {
                format!("HTTP {}", r.status)
            } else {
                r.status_text.clone()
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::file_endpoint;

    #[test]
    fn template_keys_use_the_template_route() {
        assert_eq!(
            file_endpoint("http://h:1/", "d", "01", "templates/space/MNI/t1.nii.gz"),
            "http://h:1/api/v1/templates/space/MNI/t1.nii.gz"
        );
        assert_eq!(
            file_endpoint("http://h:1", "d", "01", "/templates/t1.nii.gz"),
            "http://h:1/api/v1/templates/t1.nii.gz"
        );
    }

    #[test]
    fn dataset_keys_use_the_dataset_file_route() {
        assert_eq!(
            file_endpoint("http://h:1", "d", "01", "derivatives/recon.json"),
            "http://h:1/api/v1/datasets/d/subjects/01/files/derivatives/recon.json"
        );
    }

    #[test]
    fn atlas_keys_use_the_atlas_route() {
        assert_eq!(
            file_endpoint("http://h:1", "d", "01", "atlas/ahead/left/thalamus.obj"),
            "http://h:1/api/v1/atlas/ahead/left/thalamus.obj"
        );
    }
}
