// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! UI state machines shared by `dbsloc-app` (desktop) and `dbsloc-web` (browser).
//!
//! Everything here is pure data and pure logic: no `eframe`, no `egui`, no
//! `std::fs`, no threads, so it compiles for `wasm32`. The desktop and browser
//! front ends each render these state machines in their own widgets; the scan
//! that fills [`PipelineReadiness`] and the pipeline run itself stay in the
//! front end that can do I/O.

mod batch;
mod guided;
mod readiness;
mod settings;

pub use batch::{
    remaining_after, BatchQueue, QueuedRun, ReviewItem, ReviewMode, SubjectChecks, SubjectResult,
    NOT_RUN_STOPPED,
};
pub use dbsloc_protocol::protocol::BatchPhase;
pub use guided::{
    chosen_stimulation, guided_can_run, guided_refusal, guided_step_check, stimulation_computed,
    GuidedStep, NO_STIMULATION_HINT,
};
pub use readiness::{
    ImageKind, ImageRow, LeadReview, PipelineReadiness, RecoSideSummary, RegistrationReview,
    StageState,
};
pub use settings::{
    ants_programs_needed, coregister_engine_blocker, finer_disabled_hover, finer_selection_warning,
    finer_than_voxel, grid_size_text, norm_custom_range, normalize_engine_blocker,
    precoreg_engine_blocker, precoreg_needed, preprocess_engine_blocker, registration_method_label,
    stage_status, stage_status_detail, stages_with_precoreg, stepped_grid_text, MetalContrast,
    NormResolution, PostopModality, ReconMethod, RegistrationEngine, RunRequest, StageMeta,
    StageSettings, UpstreamReviewWarning, EL_MODEL_FROM_SUBJECT_HOVER, EL_MODEL_FROM_SUBJECT_LABEL,
    REVIEW_MODE_HOVER, REVIEW_MODE_LABEL, REVIEW_REGISTRATIONS_HOVER, REVIEW_REGISTRATIONS_LABEL,
    STAGE_META,
};
