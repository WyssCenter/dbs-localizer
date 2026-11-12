// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("BIDS error: {0}")]
    Bids(String),
    #[error("stage prerequisite missing: {0}")]
    Prereq(String),
    #[error("{tool} failed with {code}")]
    ToolFailed {
        tool: String,
        code: i32,
        stderr_tail: String,
    },
    #[error("cancelled by user")]
    Cancelled,
    #[error("stage {stage} failed: {message}")]
    StageFailed { stage: String, message: String },
    /// A stage committed its outputs and stopped at the checkreg QC gate, which is not approved.
    /// Distinct from a plain failure so a server can resume after the approval and so the driver
    /// can emit [`crate::protocol::RunEvent::StageAwaitingQc`]. The Display text is the exact
    /// string the gate produced before this variant existed (`checkreg.rs`).
    #[error("{stage}: qc not approved — {manifest}")]
    QcPending { stage: String, manifest: String },
    #[error("lock error: {0}")]
    Lock(String),
    #[error("{0}")]
    Message(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("I/O error: {0}")]
    IoNative(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type PipelineResult<T> = Result<T, PipelineError>;
