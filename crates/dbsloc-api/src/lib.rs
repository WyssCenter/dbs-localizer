// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Shared DTOs for the Wyss DBS Localizer GPU server and browser client (native + wasm32).
//!
//! This crate carries the JSON wire types shared by `dbsloc-server` and `dbsloc-web`. It holds no
//! filesystem or threading state: file paths are opaque [`String`] file keys relative to a dataset
//! or subject root, so the same types compile on the server and in the browser. See
//! `docs/SERVER_CLIENT_PLAN.md` §2 and §4 for the contract.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub use dbsloc_pipeline::options::RunOptions;
pub use dbsloc_protocol::protocol::RunEvent;
pub use dbsloc_protocol::reg_backend::RegBackend;
pub use dbsloc_protocol::stage::Stage;
pub use dbsloc_protocol::NormGrid;

/// The client-settable subset of [`RunOptions`].
///
/// Every field is optional: an absent field leaves the corresponding `RunOptions` value at its
/// default. The server owns `bids_dir`, `subject`, the normalization template, `headless`,
/// `checkreg` and `reg_backend`, so those are deliberately not part of this request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<Stage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm_method: Option<String>,
    /// Isotropic SyN registration grid in millimetres; absent leaves the `RunOptions` default
    /// (the template's own voxel size, [`NormGrid::Template`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm_isotropic_mm: Option<f64>,
    /// When `true`, the built options use the full template grid ([`NormGrid::Full`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm_full_grid: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub norm_multispectral: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recon_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recon_bright_metal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub el_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite_approved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on_error: Option<bool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl RunRequest {
    /// Applies every `Some` field onto `opts`; absent fields are left untouched.
    pub fn apply_to(&self, opts: &mut RunOptions) {
        if let Some(stages) = &self.stages {
            opts.stages = stages.clone();
        }
        if let Some(v) = &self.norm_method {
            opts.norm_method = v.clone();
        }
        if let Some(v) = self.norm_isotropic_mm {
            opts.norm_isotropic_mm = NormGrid::Mm(v);
        }
        if self.norm_full_grid == Some(true) {
            opts.norm_isotropic_mm = NormGrid::Full;
        }
        if let Some(v) = self.norm_multispectral {
            opts.norm_multispectral = v;
        }
        if let Some(v) = &self.recon_method {
            opts.recon_method = v.clone();
        }
        if let Some(v) = self.recon_bright_metal {
            opts.recon_bright_metal = v;
        }
        if let Some(v) = &self.el_model {
            opts.el_model = Some(v.clone());
        }
        if let Some(v) = self.overwrite_approved {
            opts.overwrite_approved = v;
        }
        if let Some(v) = self.stop_on_error {
            opts.stop_on_error = v;
        }
    }
}

/// Adapter, graphics API and self-test verdict of the server GPU (`GET /gpu`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub adapter: String,
    pub backend: String,
    pub verdict: String,
}

/// One dataset the caller may see (`GET /datasets`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetSummary {
    pub name: String,
    pub subjects: u32,
}

/// Per-stage state shown in a [`SubjectSummary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageState {
    /// An approved lock exists for the stage.
    Approved,
    /// Outputs exist but the stage has no approved lock.
    Outputs,
    /// No outputs and no lock.
    Missing,
    /// The stage is a placeholder in the pipeline.
    Stub,
}

/// One stage row of a [`SubjectSummary`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageSummary {
    pub stage: Stage,
    pub state: StageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// Resolved per-subject view (`GET /datasets/{ds}/subjects/{sid}`).
///
/// `checkreg_manifest` and `scene_manifest` are file keys, not filesystem paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectSummary {
    pub subj_id: String,
    pub anchor_modality: String,
    pub modalities: Vec<String>,
    #[serde(default)]
    pub postop_modalities: Vec<String>,
    pub stages: Vec<StageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkreg_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_manifest: Option<String>,
}

/// Lifecycle phase of a run (`GET /runs/{id}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Queued,
    Running,
    AwaitingQc,
    Done,
    Failed,
    Cancelled,
    /// The QC gate was approved and a follow-up run (`RunStatus::resumed_by`) took over.
    Resumed,
    /// A QC slide was rejected; the run ended at its gate.
    Rejected,
}

/// Status of a queued or running job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStatus {
    pub id: String,
    pub subject: String,
    pub phase: RunPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub stages_ok: u32,
    pub stages_failed: u32,
    pub stages_skipped: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The stage whose QC gate stopped the run (`awaiting_qc`, `resumed`, `rejected`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_stage: Option<String>,
    /// The id of the run that continues after the QC gate (phase `resumed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_by: Option<String>,
}

/// Name of the final SSE event of a run stream; its data is the terminal [`RunStatus`] and the
/// server closes the stream after it.
pub const SSE_END_EVENT: &str = "end";

/// One checkreg slide of the QC gate; all file references are file keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QcSlide {
    pub index: u32,
    pub modality: String,
    pub branch: String,
    pub checkreg_png: String,
    pub moving: String,
    pub fixed: String,
    pub approved: bool,
}

/// The checkreg QC manifest (`GET /datasets/{ds}/subjects/{sid}/qc/checkreg`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QcManifest {
    pub slides: Vec<QcSlide>,
}

/// One file announced in an upload-init request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadFileRequest {
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// `POST /datasets/{ds}/uploads` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadInitRequest {
    pub files: Vec<UploadFileRequest>,
}

/// `POST /datasets/{ds}/uploads` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadInitResponse {
    pub upload_id: String,
    pub chunk_size: u64,
}

/// One BIDS placement proposed at upload commit.
///
/// `session` is `"preop"` or `"postop"` and required at commit; `modality` is the datatype
/// directory and only `"anat"` is accepted; `acq` is `"ax"`, `"cor"`, `"sag"` or `"iso"` and is
/// required for a post-op MR image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BidsMappingEntry {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub modality: String,
    pub suffix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acq: Option<String>,
}

/// One NIfTI series produced from a DICOM upload (`POST /uploads/{id}/convert`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvertedSeries {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series_number: Option<u32>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub modality: String,
    pub dims: [u32; 3],
    #[serde(default)]
    pub voxel_mm: [f32; 3],
    pub mapping: BidsMappingEntry,
}

/// `POST /uploads/{id}/commit` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadCommitRequest {
    pub subject: String,
    pub mapping: Vec<BidsMappingEntry>,
    #[serde(default)]
    pub replace_existing: bool,
}

/// Per-slide QC approval body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub ok: bool,
}

/// Response of a per-slide QC approval.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprovalResponse {
    /// Every slide of the manifest is approved.
    pub all_approved: bool,
    /// The run submitted for the stages after the gate, when this approval completed the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_run: Option<String>,
}

/// Stage lock approval body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRequest {
    pub approved: bool,
}

/// Typed API error, serialised as `{code, message}`; `code` uses the pipeline's `E_*` codes
/// where they exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        ApiError {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_to_sets_requested_fields_and_leaves_unset_at_defaults() {
        let mut opts = RunOptions::new("/data", "01");
        let req = RunRequest {
            stages: Some(vec![Stage::Preprocess, Stage::Coregister]),
            norm_method: Some("ants_syn".to_owned()),
            norm_isotropic_mm: Some(1.5),
            recon_method: Some("pacer".to_owned()),
            recon_bright_metal: Some(true),
            el_model: Some("Boston Scientific Vercise".to_owned()),
            overwrite_approved: Some(true),
            stop_on_error: Some(false),
            ..RunRequest::default()
        };

        req.apply_to(&mut opts);

        assert_eq!(opts.stages, vec![Stage::Preprocess, Stage::Coregister]);
        assert_eq!(opts.norm_method, "ants_syn");
        assert_eq!(opts.norm_isotropic_mm, NormGrid::Mm(1.5));
        assert_eq!(opts.recon_method, "pacer");
        assert!(opts.recon_bright_metal);
        assert_eq!(opts.el_model.as_deref(), Some("Boston Scientific Vercise"));
        assert!(opts.overwrite_approved);
        assert!(!opts.stop_on_error);

        let base = RunOptions::new("/data", "01");
        assert_eq!(opts.norm_multispectral, base.norm_multispectral);
        assert_eq!(opts.reg_backend, base.reg_backend);
        assert_eq!(opts.headless, base.headless);
        assert_eq!(opts.checkreg, base.checkreg);
        assert_eq!(opts.bids_dir, base.bids_dir);
        assert_eq!(opts.subject, base.subject);
    }

    #[test]
    fn empty_request_changes_nothing() {
        let mut opts = RunOptions::new("/data", "01");
        let base = opts.clone();
        RunRequest::default().apply_to(&mut opts);
        assert_eq!(opts.stages, base.stages);
        assert_eq!(opts.norm_isotropic_mm, base.norm_isotropic_mm);
        assert_eq!(opts.recon_method, base.recon_method);
        assert_eq!(opts.recon_bright_metal, base.recon_bright_metal);
        assert_eq!(opts.el_model, base.el_model);
        assert_eq!(opts.overwrite_approved, base.overwrite_approved);
        assert_eq!(opts.stop_on_error, base.stop_on_error);
    }

    #[test]
    fn norm_full_grid_overrides_the_millimetre_grid() {
        let mut opts = RunOptions::new("/data", "01");
        assert_eq!(opts.norm_isotropic_mm, NormGrid::Template);
        let req = RunRequest {
            norm_isotropic_mm: Some(1.5),
            norm_full_grid: Some(true),
            ..RunRequest::default()
        };
        req.apply_to(&mut opts);
        assert_eq!(opts.norm_isotropic_mm, NormGrid::Full);
    }

    #[test]
    fn an_absent_grid_keeps_the_template_default() {
        let mut opts = RunOptions::new("/data", "01");
        RunRequest {
            norm_full_grid: Some(false),
            ..RunRequest::default()
        }
        .apply_to(&mut opts);
        assert_eq!(opts.norm_isotropic_mm, NormGrid::Template);
    }

    #[test]
    fn absent_fields_round_trip_through_json_and_apply() {
        let text = r#"{"norm_isotropic_mm": 2.0, "norm_full_grid": true}"#;
        let req: RunRequest = serde_json::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(req.norm_isotropic_mm, Some(2.0));
        assert_eq!(req.norm_full_grid, Some(true));
        assert!(req.stages.is_none());

        let mut opts = RunOptions::new("/data", "01");
        req.apply_to(&mut opts);
        assert_eq!(opts.norm_isotropic_mm, NormGrid::Full);
    }

    #[test]
    fn skip_serializing_if_omits_none_fields() {
        let json = serde_json::to_string(&RunRequest::default())
            .unwrap_or_else(|e| panic!("serialise: {e}"));
        assert_eq!(json, "{}");
    }

    #[test]
    fn api_error_display_and_new() {
        let e = ApiError::new("E_DUPL_RAW", "duplicate raw file");
        assert_eq!(e.code, "E_DUPL_RAW");
        assert_eq!(e.message, "duplicate raw file");
        assert_eq!(e.to_string(), "E_DUPL_RAW: duplicate raw file");
    }

    #[test]
    fn stage_state_and_run_phase_use_snake_case() {
        let state =
            serde_json::to_string(&StageState::Stub).unwrap_or_else(|e| panic!("serialise: {e}"));
        assert_eq!(state, "\"stub\"");
        let phase = serde_json::to_string(&RunPhase::AwaitingQc)
            .unwrap_or_else(|e| panic!("serialise: {e}"));
        assert_eq!(phase, "\"awaiting_qc\"");
    }
}
