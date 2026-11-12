// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The `eframe::App`: application state, the API-poll loop and the screen dispatch.
//!
//! Server data drives every screen (`docs/SERVER_CLIENT_PLAN.md` §5). The guided / card state
//! machines come from [`dbsloc_ui`]; this module holds the browser-side session state, tracks the
//! current run (status polling plus the SSE stream) and maps replies onto the screens. The pure
//! decisions live in [`crate::logic`] and [`crate::track`].

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;

use dbsloc_api::{
    ApiError, ApprovalResponse, BidsMappingEntry, ConvertedSeries, DatasetSummary, GpuInfo,
    QcManifest, RunEvent, RunPhase, RunStatus, Stage, SubjectSummary, UploadCommitRequest,
    UploadFileRequest, UploadInitRequest,
};
use dbsloc_ui::{GuidedStep, PipelineReadiness, StageSettings};

use crate::api::{ApiClient, ApiMsg};
use crate::files::{PickSlot, PickedFile};
use crate::logic::{
    build_run_request, gpu_is_usable, is_dicom_name, propose_bids_entry, readiness_from_summary,
};
use crate::screens;
use crate::sse::SseClient;
use crate::track::{is_active, RunTracker};

const REFRESH_MIN_INTERVAL_MS: f64 = 2000.0;
const E_RUN_ACTIVE: &str = "E_RUN_ACTIVE";

fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// The screen the user is on, in the order they meet them (`docs/SERVER_CLIENT_PLAN.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    SignIn,
    Landing,
    Upload,
    Guided,
    Progress,
    Qc,
    Review,
}

impl Screen {
    pub fn label(self) -> &'static str {
        match self {
            Screen::SignIn => "Sign in",
            Screen::Landing => "Landing",
            Screen::Upload => "Upload",
            Screen::Guided => "Guided run",
            Screen::Progress => "Progress",
            Screen::Qc => "Quality control",
            Screen::Review => "Review",
        }
    }
}

/// The small slice of state persisted to `localStorage` via eframe. No PHI: only the server
/// base URL and the last dataset label the user chose (`docs/SERVER_CLIENT_PLAN.md` §5).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PersistedState {
    pub base_url: String,
    #[serde(default)]
    pub last_dataset: Option<String>,
}

/// Where an upload is: each phase has one asynchronous step in flight, except `Idle` and `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UploadPhase {
    #[default]
    Idle,
    Hashing,
    Uploading,
    Converting,
    Ready,
}

/// Upload staging state (chunked PUT with a progress bar; `docs/SERVER_CLIENT_PLAN.md` §4.2).
///
/// `generation` changes whenever the upload is restarted or replaced, so a late reply of an
/// abandoned attempt is ignored.
#[derive(Default)]
pub(crate) struct UploadState {
    pub files: Vec<PickedFile>,
    pub subject: String,
    pub mapping: Vec<BidsMappingEntry>,
    pub series: Vec<ConvertedSeries>,
    pub upload_id: Option<String>,
    pub chunk_size: u64,
    pub file_index: usize,
    pub chunk_index: u64,
    pub sent_bytes: u64,
    pub hashed_bytes: u64,
    pub total_bytes: u64,
    pub phase: UploadPhase,
    pub committing: bool,
    pub replace_existing: bool,
    pub generation: u64,
    pub message: String,
}

impl UploadState {
    pub fn busy(&self) -> bool {
        self.committing
            || matches!(
                self.phase,
                UploadPhase::Hashing | UploadPhase::Uploading | UploadPhase::Converting
            )
    }

    pub fn progress(&self) -> f32 {
        let done = match self.phase {
            UploadPhase::Hashing => self.hashed_bytes,
            _ => self.sent_bytes,
        };
        if self.total_bytes == 0 {
            0.0
        } else {
            (done as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        }
    }

    pub fn progress_label(&self) -> &'static str {
        match self.phase {
            UploadPhase::Idle => "Not started",
            UploadPhase::Hashing => "Checking files",
            UploadPhase::Uploading => "Uploading",
            UploadPhase::Converting => "Converting DICOM",
            UploadPhase::Ready => "Uploaded",
        }
    }

    pub fn has_dicom(&self) -> bool {
        self.files.iter().any(|f| is_dicom_name(&f.name))
    }
}

/// QC review state: the server manifest, the shared [`CheckregReviewState`], the slide PNG keys
/// already requested (so a frame does not re-request) and the keys that failed with their error.
///
/// [`CheckregReviewState`]: dbsloc_viz::checkreg_view::CheckregReviewState
#[derive(Default)]
pub(crate) struct QcState {
    pub manifest: Option<QcManifest>,
    pub review: Option<dbsloc_viz::checkreg_view::CheckregReviewState>,
    pub requested: HashSet<String>,
    pub failed: HashMap<String, String>,
}

impl QcState {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn slide_keys(manifest: &QcManifest) -> HashSet<&str> {
        manifest
            .slides
            .iter()
            .map(|s| s.checkreg_png.as_str())
            .collect()
    }
}

/// Review state: what the byte loaders returned, and which file keys are in flight.
#[derive(Default)]
pub(crate) struct ReviewState {
    pub notes: Vec<String>,
    pub downloads: Vec<String>,
    pub template_key: Option<String>,
    pub reco_key: Option<String>,
    pub template_dims: Option<(usize, usize, usize)>,
    /// Per-hemisphere MNI geometry parsed from the fetched Reco v2 JSON by
    /// [`dbsloc_viz::reco_json::load_mni_side_geometry_from_bytes`].
    pub reco_sides: Vec<(String, dbsloc_viz::reco_json::MniSideGeometry)>,
}

#[derive(Default)]
struct SubjectRefresh {
    wanted: bool,
    in_flight: bool,
    last_at: f64,
}

/// The browser client application.
pub struct WebApp {
    pub(crate) api: ApiClient,
    pub(crate) tx: Sender<ApiMsg>,
    pub(crate) rx: Receiver<ApiMsg>,
    pub(crate) ctx: egui::Context,
    pub(crate) screen: Screen,
    pub(crate) persisted: PersistedState,
    pub(crate) base_url_input: String,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
    pub(crate) gpu: Option<GpuInfo>,
    pub(crate) gpu_ok: Option<bool>,
    pub(crate) datasets: Vec<DatasetSummary>,
    pub(crate) selected_dataset: Option<String>,
    pub(crate) subjects: Vec<String>,
    pub(crate) selected_subject: Option<String>,
    pub(crate) summary: Option<SubjectSummary>,
    pub(crate) readiness: PipelineReadiness,
    pub(crate) settings: StageSettings,
    pub(crate) step: GuidedStep,
    pub(crate) plan_json: Option<serde_json::Value>,
    pub(crate) run_id: Option<String>,
    pub(crate) run: Option<RunStatus>,
    pub(crate) log: Vec<String>,
    pub(crate) sse: Option<SseClient>,
    pub(crate) qc: QcState,
    pub(crate) textures: HashMap<String, egui::TextureHandle>,
    pub(crate) upload: UploadState,
    pub(crate) review: ReviewState,
    pub(crate) recent_subjects: Vec<String>,
    pub(crate) pick_slot: PickSlot,
    tracker: RunTracker,
    refresh: SubjectRefresh,
}

fn page_origin() -> String {
    web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}

impl WebApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut persisted = cc
            .storage
            .and_then(|s| eframe::get_value::<PersistedState>(s, "dbsloc_web"))
            .unwrap_or_default();
        if persisted.base_url.trim().is_empty() {
            persisted.base_url = page_origin();
        }
        let (tx, rx) = channel();
        let ctx = cc.egui_ctx.clone();
        let api = ApiClient::new(persisted.base_url.clone(), tx.clone(), ctx.clone());
        Self {
            api,
            tx,
            rx,
            ctx,
            screen: Screen::SignIn,
            base_url_input: persisted.base_url.clone(),
            persisted,
            status: String::new(),
            error: None,
            gpu: None,
            gpu_ok: None,
            datasets: Vec::new(),
            selected_dataset: None,
            subjects: Vec::new(),
            selected_subject: None,
            summary: None,
            readiness: PipelineReadiness::default(),
            settings: StageSettings::default(),
            step: GuidedStep::FIRST_WORK,
            plan_json: None,
            run_id: None,
            run: None,
            log: Vec::new(),
            sse: None,
            qc: QcState::default(),
            textures: HashMap::new(),
            upload: UploadState::default(),
            review: ReviewState::default(),
            recent_subjects: Vec::new(),
            pick_slot: PickSlot::default(),
            tracker: RunTracker::new(),
            refresh: SubjectRefresh::default(),
        }
    }

    /// Sign in (v1: a base URL and *Continue*; no auth, `docs/SERVER_CLIENT_PLAN.md` D2).
    pub(crate) fn connect(&mut self) {
        let mut url = self.base_url_input.trim().to_owned();
        if url.is_empty() {
            url = page_origin();
            self.base_url_input = url.clone();
        }
        self.persisted.base_url = url;
        self.api = ApiClient::new(
            self.persisted.base_url.clone(),
            self.tx.clone(),
            self.ctx.clone(),
        );
        self.screen = Screen::Landing;
        self.status = "Connecting to server…".to_owned();
        self.error = None;
        self.api.get_gpu();
        self.api.get_datasets();
    }

    pub(crate) fn select_dataset(&mut self, name: &str) {
        self.reset_subject_state();
        self.selected_dataset = Some(name.to_owned());
        self.persisted.last_dataset = Some(name.to_owned());
        self.subjects.clear();
        self.status = format!("Loading subjects of {name}…");
        self.api.get_subjects(name);
    }

    pub(crate) fn open_subject(&mut self, sid: &str) {
        self.reset_subject_state();
        self.selected_subject = Some(sid.to_owned());
        self.status = format!("Loading subject {sid}…");
        if !self.recent_subjects.iter().any(|s| s == sid) {
            self.recent_subjects.insert(0, sid.to_owned());
            self.recent_subjects.truncate(8);
        }
        if let Some(ds) = self.selected_dataset.clone() {
            self.api.get_subject(&ds, sid, true);
            self.api.get_active_run(&ds, sid);
        }
    }

    fn reset_subject_state(&mut self) {
        self.sse = None;
        self.run_id = None;
        self.run = None;
        self.log.clear();
        self.tracker = RunTracker::new();
        self.qc.reset();
        self.textures.clear();
        self.review = ReviewState::default();
        self.plan_json = None;
        self.summary = None;
        self.selected_subject = None;
        self.refresh = SubjectRefresh::default();
        self.readiness = PipelineReadiness::default();
    }

    fn is_current_subject(&self, ds: &str, sid: &str) -> bool {
        self.selected_dataset.as_deref() == Some(ds)
            && self.selected_subject.as_deref() == Some(sid)
    }

    fn refresh_readiness(&mut self) {
        if let Some(s) = &self.summary {
            self.readiness = readiness_from_summary(s, self.gpu_ok);
        }
    }

    fn run_is_active(&self) -> bool {
        self.run_id.is_some() && self.run.as_ref().is_none_or(|r| is_active(r.phase))
    }

    pub(crate) fn start_step(&mut self, stage: Stage) {
        let req = build_run_request(&self.settings, vec![stage]);
        if let (Some(ds), Some(sid)) =
            (self.selected_dataset.clone(), self.selected_subject.clone())
        {
            self.status = format!("Submitting {} run…", stage.label());
            self.error = None;
            self.api.post_run(&ds, &sid, &req);
        }
    }

    pub(crate) fn request_plan(&mut self) {
        if let (Some(ds), Some(sid)) =
            (self.selected_dataset.clone(), self.selected_subject.clone())
        {
            self.api.post_plan(&ds, &sid);
        }
    }

    /// Start watching run `id`: status polling plus the event stream. `keep_log` continues the
    /// log of the run it resumes.
    fn track_run(&mut self, id: String, keep_log: bool) {
        self.sse = None;
        self.tracker = RunTracker::new();
        self.run = None;
        if keep_log {
            self.log.push(format!("-- continuing as run {id}"));
        } else {
            self.log.clear();
        }
        self.qc.reset();
        self.textures.clear();
        self.status.clear();
        self.run_id = Some(id.clone());
        self.api.get_run(&id);
        self.tracker.poll_sent(now_ms());
        self.open_stream(&id);
        self.screen = Screen::Progress;
    }

    fn open_stream(&mut self, id: &str) {
        match SseClient::connect(&self.api, id, self.tracker.last_event_id()) {
            Ok(client) => self.sse = Some(client),
            Err(e) => {
                self.log.push(format!("event stream unavailable: {e}"));
                self.tracker.stream_lost(now_ms());
            }
        }
    }

    fn tick(&mut self) {
        let now = now_ms();
        if let Some(id) = self.run_id.clone() {
            if self.run_is_active() {
                if self.tracker.poll_due(now) {
                    self.tracker.poll_sent(now);
                    self.api.get_run(&id);
                }
                if self.sse.is_none() && self.tracker.reconnect_due(now) {
                    self.tracker.reconnect_started();
                    self.open_stream(&id);
                }
            }
        }
        if self.refresh.wanted
            && !self.refresh.in_flight
            && now - self.refresh.last_at >= REFRESH_MIN_INTERVAL_MS
        {
            if let (Some(ds), Some(sid)) =
                (self.selected_dataset.clone(), self.selected_subject.clone())
            {
                self.refresh.wanted = false;
                self.refresh.in_flight = true;
                self.refresh.last_at = now;
                self.api.get_subject(&ds, &sid, false);
            } else {
                self.refresh.wanted = false;
            }
        }
    }

    fn wants_timer(&self) -> bool {
        self.run_is_active() || self.upload.busy() || self.refresh.wanted || self.refresh.in_flight
    }

    fn apply_run_status(&mut self, st: RunStatus) {
        let prev = self.run.as_ref().map(|r| r.phase);
        let phase = st.phase;
        let resumed_by = st.resumed_by.clone();
        self.run = Some(st);
        if !is_active(phase) {
            self.sse = None;
            self.tracker.stream_ended();
        }
        match phase {
            RunPhase::AwaitingQc => {
                if prev != Some(RunPhase::AwaitingQc) {
                    self.qc.reset();
                    self.textures.clear();
                    self.request_qc();
                    self.screen = Screen::Qc;
                }
            }
            RunPhase::Done => {
                self.refresh.wanted = true;
                self.populate_downloads();
                if prev != Some(RunPhase::Done) {
                    self.screen = Screen::Review;
                }
            }
            RunPhase::Failed | RunPhase::Cancelled | RunPhase::Rejected => {
                self.refresh.wanted = true;
                if self.screen == Screen::Qc {
                    self.screen = Screen::Progress;
                }
            }
            RunPhase::Resumed => {
                self.refresh.wanted = true;
                if let Some(next) = resumed_by {
                    if self.run_id.as_deref() != Some(next.as_str()) {
                        self.track_run(next, true);
                    }
                }
            }
            RunPhase::Queued | RunPhase::Running => {}
        }
    }

    fn apply_run_event(&mut self, ev: RunEvent) {
        match &ev {
            RunEvent::LogLine { line } => self.log.push(line.clone()),
            RunEvent::StageStarted { stage } => self.log.push(format!("> {stage}")),
            RunEvent::StageCompleted { stage, .. } => {
                self.log.push(format!("ok  {stage}"));
                self.refresh.wanted = true;
            }
            RunEvent::StageSkipped { stage, reason, .. } => {
                self.log.push(format!("skip {stage}: {reason}"))
            }
            RunEvent::StageFailed { stage, message } => {
                self.log.push(format!("error {stage}: {message}"));
                self.tracker.poll_soon();
            }
            RunEvent::StageAwaitingQc { stage } => {
                self.log.push(format!("awaiting QC: {stage}"));
                self.tracker.poll_soon();
            }
            RunEvent::PipelineCompleted { .. } => {
                self.log.push("pipeline completed".to_owned());
                self.tracker.poll_soon();
            }
        }
        if self.log.len() > 2000 {
            let drain = self.log.len() - 2000;
            self.log.drain(0..drain);
        }
    }

    fn populate_downloads(&mut self) {
        self.review.downloads.clear();
        if let Some(s) = &self.summary {
            if let Some(k) = &s.checkreg_manifest {
                self.review.downloads.push(k.clone());
            }
            if let Some(k) = &s.scene_manifest {
                self.review.downloads.push(k.clone());
            }
        }
        if let Some(m) = &self.qc.manifest {
            for slide in &m.slides {
                if !slide.checkreg_png.is_empty() {
                    self.review.downloads.push(slide.checkreg_png.clone());
                }
            }
        }
    }

    pub(crate) fn request_qc(&mut self) {
        if let (Some(ds), Some(sid)) =
            (self.selected_dataset.clone(), self.selected_subject.clone())
        {
            self.api.get_qc(&ds, &sid);
        }
    }

    pub(crate) fn request_slide(&mut self, key: &str) {
        if let (Some(ds), Some(sid)) =
            (self.selected_dataset.clone(), self.selected_subject.clone())
        {
            self.qc.requested.insert(key.to_owned());
            self.api.get_file(&ds, &sid, key);
        }
    }

    pub(crate) fn retry_slide(&mut self, key: &str) {
        self.qc.failed.remove(key);
        self.qc.requested.remove(key);
    }

    fn apply_qc_manifest(&mut self, manifest: QcManifest) {
        let n = manifest.slides.len();
        let changed = self
            .qc
            .manifest
            .as_ref()
            .is_none_or(|old| QcState::slide_keys(old) != QcState::slide_keys(&manifest));
        if changed {
            self.textures.clear();
            self.qc.requested.clear();
            self.qc.failed.clear();
        }
        let keep_review = self
            .qc
            .review
            .as_ref()
            .is_some_and(|r| r.slide_count() == n);
        if !keep_review {
            self.qc.review = Some(dbsloc_viz::checkreg_view::CheckregReviewState::new(n));
        }
        self.qc.manifest = Some(manifest);
        self.status.clear();
    }

    fn apply_approval(&mut self, slide: u32, response: ApprovalResponse) {
        self.log.push(format!(
            "slide {} decided (all approved: {})",
            slide + 1,
            response.all_approved
        ));
        match response.resumed_run {
            Some(next) => self.track_run(next, true),
            None => self.request_qc(),
        }
    }

    pub(crate) fn pick_files(&mut self) {
        crate::files::pick_files(self.pick_slot.clone(), self.ctx.clone());
    }

    fn on_files_picked(&mut self, files: Vec<PickedFile>) {
        let generation = self.upload.generation + 1;
        self.upload = UploadState {
            generation,
            subject: std::mem::take(&mut self.upload.subject),
            total_bytes: files.iter().map(|f| f.size).sum(),
            mapping: files
                .iter()
                .filter(|f| !is_dicom_name(&f.name))
                .map(|f| propose_bids_entry(&f.name))
                .collect(),
            files,
            ..UploadState::default()
        };
    }

    pub(crate) fn begin_upload(&mut self) {
        let Some(_) = self.selected_dataset.clone() else {
            self.error = Some("Choose a dataset before uploading.".to_owned());
            return;
        };
        if self.upload.files.is_empty() {
            self.error = Some("Choose one or more NIfTI or DICOM files first.".to_owned());
            return;
        }
        self.upload.generation += 1;
        self.upload.upload_id = None;
        self.upload.sent_bytes = 0;
        self.upload.hashed_bytes = 0;
        self.upload.file_index = 0;
        self.upload.chunk_index = 0;
        self.upload.phase = UploadPhase::Hashing;
        self.upload.message.clear();
        crate::files::hash_files(
            self.api.clone(),
            self.upload.generation,
            self.upload.files.clone(),
        );
    }

    fn init_upload(&mut self, hashes: Vec<String>) {
        let Some(ds) = self.selected_dataset.clone() else {
            return;
        };
        let req = UploadInitRequest {
            files: self
                .upload
                .files
                .iter()
                .zip(hashes)
                .map(|(f, sha256)| UploadFileRequest {
                    name: f.name.clone(),
                    size: f.size,
                    sha256: Some(sha256),
                })
                .collect(),
        };
        self.upload.phase = UploadPhase::Uploading;
        self.api.init_upload(self.upload.generation, &ds, &req);
    }

    fn send_next_chunk(&mut self) {
        let Some(upload_id) = self.upload.upload_id.clone() else {
            return;
        };
        loop {
            let Some(file) = self.upload.files.get(self.upload.file_index) else {
                self.finish_chunks(&upload_id);
                return;
            };
            match crate::logic::chunk_range(
                file.size,
                self.upload.chunk_size,
                self.upload.chunk_index,
            ) {
                Some((start, end)) => {
                    crate::files::send_chunk(
                        self.api.clone(),
                        self.upload.generation,
                        upload_id,
                        file.clone(),
                        self.upload.chunk_index,
                        start,
                        end,
                    );
                    return;
                }
                None => {
                    self.upload.file_index += 1;
                    self.upload.chunk_index = 0;
                }
            }
        }
    }

    fn finish_chunks(&mut self, upload_id: &str) {
        if self.upload.has_dicom() {
            self.upload.phase = UploadPhase::Converting;
            self.upload.message = "Converting the DICOM series on the server…".to_owned();
            self.api.convert_upload(self.upload.generation, upload_id);
        } else {
            self.upload.phase = UploadPhase::Ready;
            self.upload.message =
                "All chunks uploaded. Review the BIDS mapping and commit.".to_owned();
        }
    }

    fn apply_series(&mut self, series: Vec<ConvertedSeries>) {
        self.upload.phase = UploadPhase::Ready;
        if series.is_empty() {
            self.upload.message = "No DICOM series were found in the upload.".to_owned();
            return;
        }
        self.upload
            .mapping
            .extend(series.iter().map(|s| s.mapping.clone()));
        self.upload.message = format!(
            "{} series converted. Review the proposed BIDS mapping and commit.",
            series.len()
        );
        self.upload.series = series;
    }

    pub(crate) fn commit_upload(&mut self) {
        let Some(id) = self.upload.upload_id.clone() else {
            self.error = Some("Upload has not started.".to_owned());
            return;
        };
        let subject = self.upload.subject.trim().to_owned();
        if subject.is_empty() {
            self.error = Some("Enter a subject id before committing.".to_owned());
            return;
        }
        self.upload.committing = true;
        let req = UploadCommitRequest {
            subject,
            mapping: self.upload.mapping.clone(),
            replace_existing: self.upload.replace_existing,
        };
        self.api.commit_upload(self.upload.generation, &id, &req);
    }

    fn finish_commit(&mut self) {
        let subject = self.upload.subject.trim().to_owned();
        let generation = self.upload.generation + 1;
        self.upload = UploadState {
            generation,
            message: format!("Committed subject {subject}."),
            ..UploadState::default()
        };
        if let Some(ds) = self.selected_dataset.clone() {
            self.api.get_subjects(&ds);
        }
        self.api.get_datasets();
    }

    pub(crate) fn load_scene(&mut self) {
        if self
            .summary
            .as_ref()
            .is_none_or(|s| s.scene_manifest.is_none())
        {
            self.review.notes.push(
                "No scene manifest for this subject yet — run Reconstruct (and Visualize) first."
                    .to_owned(),
            );
            return;
        }
        let (Some(ds), Some(sid)) = (self.selected_dataset.clone(), self.selected_subject.clone())
        else {
            return;
        };
        self.review.notes.clear();
        self.review.template_dims = None;
        self.review.reco_sides.clear();
        self.review.template_key = None;
        self.review.reco_key = None;
        self.review
            .notes
            .push("Fetching the scene manifest".to_owned());
        self.api.get_scene(&ds, &sid);
    }

    fn apply_scene(&mut self, ds: &str, sid: &str, manifest: &serde_json::Value) {
        let key_of = |name: &str| {
            manifest
                .get(name)
                .and_then(|x| x.as_str())
                .filter(|k| !k.is_empty())
                .map(str::to_owned)
        };
        self.review
            .notes
            .push("Scene manifest received (server file keys).".to_owned());
        if let Some(t) = key_of("template_nii") {
            self.review.template_key = Some(t.clone());
            self.api.get_file(ds, sid, &t);
        } else {
            self.review
                .notes
                .push("Scene manifest has no template.".to_owned());
        }
        if let Some(r) = key_of("reco_json") {
            self.review.reco_key = Some(r.clone());
            self.api.get_file(ds, sid, &r);
        } else {
            self.review
                .notes
                .push("Scene manifest has no reconstruction.".to_owned());
        }
        let atlas = manifest
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, v)| {
                        k.starts_with("atlas") && v.as_str().is_some_and(|s| !s.is_empty())
                    })
                    .count()
            })
            .unwrap_or(0);
        if atlas > 0 {
            self.review.notes.push(format!(
                "{atlas} atlas key(s) in the manifest (drawn by the 3D viewer)."
            ));
        }
    }

    fn is_review_key(&self, key: &str) -> bool {
        self.review.template_key.as_deref() == Some(key)
            || self.review.reco_key.as_deref() == Some(key)
    }

    /// Route a fetched file to the review loaders when its key is one the review screen asked
    /// for. Returns `true` when it was consumed here.
    fn handle_review_file(&mut self, key: &str, bytes: &[u8]) -> bool {
        if self.review.template_key.as_deref() == Some(key) {
            match dbsloc_viz::scene_volume::TemplateVolume::from_bytes(bytes) {
                Ok(v) => {
                    self.review.template_dims = Some((v.n0, v.n1, v.n2));
                    self.review.notes.push(format!(
                        "Template volume loaded: {}×{}×{}",
                        v.n0, v.n1, v.n2
                    ));
                }
                Err(e) => self.review.notes.push(format!("Template load error: {e}")),
            }
            return true;
        }
        if self.review.reco_key.as_deref() == Some(key) {
            let mut parsed: Vec<(String, dbsloc_viz::reco_json::MniSideGeometry)> = Vec::new();
            for (label, side) in [("right", "1"), ("left", "2")] {
                match dbsloc_viz::reco_json::load_mni_side_geometry_from_bytes(bytes, side) {
                    Ok(geom) => parsed.push((label.to_owned(), geom)),
                    Err(e) => self
                        .review
                        .notes
                        .push(format!("Reco {label} side not parsed: {e}")),
                }
            }
            if parsed.is_empty() {
                self.review
                    .notes
                    .push("Reco v2 JSON fetched, but no MNI side geometry parsed.".to_owned());
            } else {
                self.review
                    .notes
                    .push(format!("Reco v2 JSON parsed ({} side(s)).", parsed.len()));
            }
            self.review.reco_sides = parsed;
            return true;
        }
        false
    }

    fn handle_file(&mut self, key: String, result: Result<Vec<u8>, ApiError>) {
        let bytes = match result {
            Ok(b) => b,
            Err(e) => {
                if self.is_review_key(&key) {
                    self.review.notes.push(format!("{key}: {e}"));
                } else {
                    self.qc.failed.insert(key, e.to_string());
                }
                return;
            }
        };
        if self.handle_review_file(&key, &bytes) {
            return;
        }
        match decode_png(&bytes) {
            Some(image) => {
                let texture =
                    self.ctx
                        .load_texture(key.clone(), image, egui::TextureOptions::LINEAR);
                self.qc.failed.remove(&key);
                self.textures.insert(key, texture);
            }
            None => {
                self.qc
                    .failed
                    .insert(key, "the slide image could not be decoded".to_owned());
            }
        }
    }

    fn poll_api(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        let picked = self.pick_slot.borrow_mut().take();
        if let Some(files) = picked {
            self.on_files_picked(files);
        }
    }

    fn handle(&mut self, msg: ApiMsg) {
        match msg {
            ApiMsg::Gpu(Ok(g)) => {
                self.gpu_ok = Some(gpu_is_usable(&g));
                self.gpu = Some(g);
                self.refresh_readiness();
            }
            ApiMsg::Gpu(Err(e)) => {
                self.gpu_ok = Some(false);
                self.error = Some(e.to_string());
                self.refresh_readiness();
            }
            ApiMsg::Datasets(Ok(d)) => {
                self.datasets = d;
                self.status.clear();
            }
            ApiMsg::Datasets(Err(e)) => self.error = Some(e.to_string()),
            ApiMsg::Subjects { ds, result } => {
                if self.selected_dataset.as_deref() != Some(ds.as_str()) {
                    return;
                }
                match result {
                    Ok(s) => {
                        self.subjects = s;
                        self.status.clear();
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            ApiMsg::Subject {
                ds,
                sid,
                open,
                result,
            } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                if !open {
                    self.refresh.in_flight = false;
                }
                match result {
                    Ok(s) => {
                        self.summary = Some(s);
                        self.refresh_readiness();
                        self.populate_downloads();
                        self.status.clear();
                        if open {
                            self.step = GuidedStep::FIRST_WORK;
                            if !self.run_is_active() {
                                self.screen = Screen::Guided;
                            }
                        }
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            ApiMsg::Plan { ds, sid, result } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(v) => self.plan_json = Some(v),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            ApiMsg::RunCreated { ds, sid, result } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(id) => self.track_run(id, false),
                    Err(e) if e.code == E_RUN_ACTIVE => {
                        self.status =
                            "A run is already active for this subject; attaching to it.".to_owned();
                        self.api.get_active_run(&ds, &sid);
                    }
                    Err(e) => {
                        self.error = Some(e.to_string());
                        self.status.clear();
                    }
                }
            }
            ApiMsg::RunStatus { id, result } => {
                if self.run_id.as_deref() != Some(id.as_str()) {
                    return;
                }
                self.tracker.poll_answered();
                match result {
                    Ok(st) => self.apply_run_status(st),
                    Err(e) => self.status = format!("Run status unavailable: {e}"),
                }
            }
            ApiMsg::ActiveRun { ds, sid, result } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(Some(st)) => {
                        if self.run_id.as_deref() != Some(st.id.as_str()) {
                            self.track_run(st.id.clone(), false);
                        }
                        self.apply_run_status(st);
                    }
                    Ok(None) => {}
                    Err(e) => self.status = format!("Active run lookup failed: {e}"),
                }
            }
            ApiMsg::Cancel { id, result } => match result {
                Ok(()) => {
                    if self.run_id.as_deref() == Some(id.as_str()) {
                        self.tracker.poll_soon();
                        self.api.get_run(&id);
                        self.tracker.poll_sent(now_ms());
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            ApiMsg::Qc { ds, sid, result } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(m) => self.apply_qc_manifest(m),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            ApiMsg::Scene { ds, sid, result } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(v) => self.apply_scene(&ds, &sid, &v),
                    Err(e) => self.review.notes.push(format!("Scene manifest: {e}")),
                }
            }
            ApiMsg::Approval {
                ds,
                sid,
                slide,
                result,
            } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                match result {
                    Ok(r) => self.apply_approval(slide, r),
                    Err(e) => self.error = Some(format!("approval of slide {}: {e}", slide + 1)),
                }
            }
            ApiMsg::HashProgress { generation, done } => {
                if generation == self.upload.generation {
                    self.upload.hashed_bytes = done;
                }
            }
            ApiMsg::HashDone { generation, result } => {
                if generation != self.upload.generation {
                    return;
                }
                match result {
                    Ok(hashes) if hashes.len() == self.upload.files.len() => {
                        self.init_upload(hashes)
                    }
                    Ok(_) => {
                        self.upload.phase = UploadPhase::Idle;
                        self.error = Some("Hashing returned an unexpected result.".to_owned());
                    }
                    Err(e) => {
                        self.upload.phase = UploadPhase::Idle;
                        self.error = Some(format!("could not read the file: {e}"));
                    }
                }
            }
            ApiMsg::UploadInit { generation, result } => {
                if generation != self.upload.generation {
                    return;
                }
                match result {
                    Ok(r) => {
                        self.upload.upload_id = Some(r.upload_id);
                        self.upload.chunk_size = r.chunk_size.max(1);
                        self.upload.file_index = 0;
                        self.upload.chunk_index = 0;
                        self.upload.sent_bytes = 0;
                        self.send_next_chunk();
                    }
                    Err(e) => {
                        self.upload.phase = UploadPhase::Idle;
                        self.error = Some(e.to_string());
                    }
                }
            }
            ApiMsg::UploadChunk {
                generation,
                file,
                chunk,
                len,
                result,
            } => {
                if generation != self.upload.generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        self.upload.sent_bytes += len;
                        self.upload.chunk_index += 1;
                        self.send_next_chunk();
                    }
                    Err(e) => {
                        self.upload.phase = UploadPhase::Idle;
                        self.error = Some(format!("upload {file} chunk {chunk}: {e}"));
                    }
                }
            }
            ApiMsg::UploadConvert { generation, result } => {
                if generation != self.upload.generation {
                    return;
                }
                match result {
                    Ok(series) => self.apply_series(series),
                    Err(e) => {
                        self.upload.phase = UploadPhase::Ready;
                        self.upload.message = "DICOM conversion failed.".to_owned();
                        self.error = Some(e.to_string());
                    }
                }
            }
            ApiMsg::UploadCommit { generation, result } => {
                if generation != self.upload.generation {
                    return;
                }
                self.upload.committing = false;
                match result {
                    Ok(()) => self.finish_commit(),
                    Err(e) => self.error = Some(e.to_string()),
                }
            }
            ApiMsg::File {
                ds,
                sid,
                key,
                result,
            } => {
                if !self.is_current_subject(&ds, &sid) {
                    return;
                }
                self.handle_file(key, result);
            }
            ApiMsg::RunEvent { run_id, id, event } => {
                if self.run_id.as_deref() == Some(run_id.as_str()) && self.tracker.accept_event(id)
                {
                    self.apply_run_event(event);
                }
            }
            ApiMsg::RunEnd { run_id, status } => {
                if self.run_id.as_deref() == Some(run_id.as_str()) {
                    self.tracker.stream_ended();
                    self.sse = None;
                    self.apply_run_status(status);
                }
            }
            ApiMsg::SseClosed { run_id } => {
                if self.run_id.as_deref() == Some(run_id.as_str()) {
                    self.sse = None;
                    self.tracker.stream_lost(now_ms());
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Wyss DBS Localizer");
            ui.separator();
            ui.label(self.screen.label());
            ui.separator();
            let (color, text) = match self.gpu_ok {
                Some(true) => (egui::Color32::LIGHT_GREEN, "GPU ready"),
                Some(false) => (egui::Color32::LIGHT_RED, "GPU unavailable"),
                None => (egui::Color32::GRAY, "GPU unknown"),
            };
            ui.colored_label(color, text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Home").clicked() {
                    self.screen = if self.datasets.is_empty() {
                        Screen::SignIn
                    } else {
                        Screen::Landing
                    };
                }
                if let Some(ds) = &self.selected_dataset {
                    ui.label(format!("dataset: {ds}"));
                }
            });
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }
}

impl eframe::App for WebApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_api();
        self.tick();

        egui::Panel::top("dbsloc_top").show(ui, |ui| self.top_bar(ui));

        if let Some(err) = self.error.clone() {
            egui::Panel::bottom("dbsloc_error").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("Error: {err}"));
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
            });
        }

        match self.screen {
            Screen::SignIn => screens::signin::show(self, ui),
            Screen::Landing => screens::landing::show(self, ui),
            Screen::Upload => screens::upload::show(self, ui),
            Screen::Guided => screens::guided::show(self, ui),
            Screen::Progress => screens::progress::show(self, ui),
            Screen::Qc => screens::qc::show(self, ui),
            Screen::Review => screens::review::show(self, ui),
        }

        if self.wants_timer() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "dbsloc_web", &self.persisted);
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}

/// Decode a PNG (the checkreg slide) into an egui texture image.
fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}
