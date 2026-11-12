// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! JSON stdin/stdout protocol for Python ↔ Rust pipeline runner communication.
//!
//! A front end passes ``RunOptions`` as JSON on stdin; Rust writes ``RunResult``
//! (one JSON object per line, streaming) to stdout.
//!
//! Moved verbatim from `dbsloc-pipeline/src/protocol.rs` so the same wire types are available to
//! the browser client without pulling in the pipeline (and its GPU dependencies).

use serde::{Deserialize, Serialize};

/// Single progress event written to stdout (newline-delimited JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    StageStarted {
        stage: String,
    },
    StageCompleted {
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// Stage ran no work and wrote no outputs — a stub, or a prerequisite absent by design.
    /// Distinct from [`RunEvent::StageCompleted`] so a stub cannot read as a finished stage.
    StageSkipped {
        stage: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    StageFailed {
        stage: String,
        message: String,
    },
    /// A stage finished its outputs but the checkreg QC gate is not approved.
    ///
    /// Emitted **before** the [`RunEvent::StageFailed`] that follows it, so a server can map it to
    /// an `awaiting_qc` run status and resume after approval, while the desktop and CLI keep the
    /// same `stage_failed` row they printed before this event existed.
    StageAwaitingQc {
        stage: String,
    },
    /// One subprocess output line (`dbsloc run` stdout/stderr) when streaming to GUI.
    LogLine {
        line: String,
    },
    PipelineCompleted {
        stages_ok: u32,
        stages_failed: u32,
        #[serde(default)]
        stages_skipped: u32,
    },
}

/// How one subject of a batch run ended (`specs/13` §10–§11 reference behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchPhase {
    Done,
    Failed,
    /// A registration waits for review; the subject's later steps did not run.
    AwaitingQc,
    Cancelled,
    /// The subject could not be resolved or planned; no step ran.
    PlanError,
}

impl BatchPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            BatchPhase::Done => "done",
            BatchPhase::Failed => "failed",
            BatchPhase::AwaitingQc => "awaiting_qc",
            BatchPhase::Cancelled => "cancelled",
            BatchPhase::PlanError => "plan_error",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BatchPhase::Done => "done",
            BatchPhase::Failed => "failed",
            BatchPhase::AwaitingQc => "awaiting review",
            BatchPhase::Cancelled => "cancelled",
            BatchPhase::PlanError => "not found",
        }
    }

    pub fn is_failure(self) -> bool {
        matches!(self, BatchPhase::Failed | BatchPhase::PlanError)
    }
}

/// What a stage body actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutcome {
    Completed(String),
    Skipped { reason: String, result: String },
}

impl StageOutcome {
    pub fn done(result: impl Into<String>) -> Self {
        StageOutcome::Completed(result.into())
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        StageOutcome::Skipped {
            result: format!("skip:{reason}"),
            reason,
        }
    }

    pub fn skipped_as(reason: impl Into<String>, result: impl Into<String>) -> Self {
        StageOutcome::Skipped {
            reason: reason.into(),
            result: result.into(),
        }
    }
}

pub fn emit(event: &RunEvent) {
    let _ = try_emit(event);
}

pub fn try_emit(event: &RunEvent) -> std::io::Result<()> {
    use std::io::Write;
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()
}
