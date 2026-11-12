// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Conversions from pipeline structs and persisted jobs into the shared `dbsloc_api` DTOs.

use std::path::Path;

use dbsloc_api::{
    QcManifest, QcSlide, RunPhase, RunStatus, StageState, StageSummary, SubjectSummary,
};
use dbsloc_pipeline::checkreg::slide_approved;
use dbsloc_pipeline::stages::Stage;
use dbsloc_pipeline::subject_summary::SubjectSummaryData;

use crate::db::JobRow;
use crate::files::file_key;
use crate::timeutil::rfc3339_from_millis;

/// Map a `SubjectSummaryData` into the wire DTO, rewriting the manifest paths to dataset file keys.
pub fn subject_summary(data: &SubjectSummaryData, dataset_root: &Path) -> SubjectSummary {
    SubjectSummary {
        subj_id: data.subj_id.clone(),
        anchor_modality: data.anchor_modality.clone(),
        modalities: data.modalities.clone(),
        postop_modalities: data.postop_modalities.clone(),
        stages: data.stages.iter().filter_map(stage_summary).collect(),
        checkreg_manifest: data
            .checkreg_manifest
            .as_deref()
            .and_then(|p| file_key(dataset_root, Path::new(p))),
        scene_manifest: data
            .scene_manifest
            .as_deref()
            .and_then(|p| file_key(dataset_root, Path::new(p))),
    }
}

fn stage_summary(row: &dbsloc_pipeline::subject_summary::StageSummaryData) -> Option<StageSummary> {
    Some(StageSummary {
        stage: Stage::parse(&row.stage)?,
        state: stage_state(&row.state),
        result: row.result.clone(),
    })
}

/// `"approved" | "outputs" | "stub" | "missing"` → [`StageState`].
pub fn stage_state(s: &str) -> StageState {
    match s {
        "approved" => StageState::Approved,
        "outputs" => StageState::Outputs,
        "stub" => StageState::Stub,
        _ => StageState::Missing,
    }
}

/// Build a [`QcManifest`] from a parsed checkreg manifest value, turning absolute paths into
/// dataset file keys and reading each slide's approval from its method log.
///
/// The manifest slide fields are `{checkreg_png, moving, fixed, branch, modality, method_log}`.
pub fn qc_manifest(manifest: &serde_json::Value, dataset_root: &Path) -> QcManifest {
    let mut slides = Vec::new();
    let Some(rows) = manifest.get("slides").and_then(|v| v.as_array()) else {
        return QcManifest { slides };
    };
    for (index, row) in rows.iter().enumerate() {
        let str_field = |k: &str| row.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        let key = |k: &str| {
            let raw = str_field(k);
            if raw.is_empty() {
                String::new()
            } else {
                file_key(dataset_root, Path::new(raw)).unwrap_or_default()
            }
        };
        let method_log = str_field("method_log");
        let modality = Some(str_field("modality")).filter(|s| !s.is_empty());
        let approved = !method_log.is_empty()
            && slide_approved(Path::new(method_log), str_field("branch"), modality);
        slides.push(QcSlide {
            index: index as u32,
            modality: str_field("modality").to_owned(),
            branch: str_field("branch").to_owned(),
            checkreg_png: key("checkreg_png"),
            moving: key("moving"),
            fixed: key("fixed"),
            approved,
        });
    }
    QcManifest { slides }
}

/// Map a job row into a [`RunStatus`].
pub fn run_status(job: &JobRow) -> RunStatus {
    let now = crate::timeutil::now_unix_millis();
    let elapsed_ms = job.started_at.map(|s| {
        let end = job.finished_at.unwrap_or(now);
        (end.saturating_sub(s)).max(0) as u64
    });
    RunStatus {
        id: job.id.clone(),
        subject: job.subject.clone(),
        phase: run_phase(&job.phase),
        stage: job.stage.clone(),
        stages_ok: job.stages_ok,
        stages_failed: job.stages_failed,
        stages_skipped: job.stages_skipped,
        started_at: job.started_at.map(rfc3339_from_millis),
        finished_at: job.finished_at.map(rfc3339_from_millis),
        elapsed_ms,
        message: job.message.clone(),
        gated_stage: job.gated_stage.clone(),
        resumed_by: job.resumed_by.clone(),
    }
}

/// `"queued" | "running" | …` → [`RunPhase`].
pub fn run_phase(phase: &str) -> RunPhase {
    match phase {
        "running" => RunPhase::Running,
        "awaiting_qc" => RunPhase::AwaitingQc,
        "done" => RunPhase::Done,
        "failed" => RunPhase::Failed,
        "cancelled" => RunPhase::Cancelled,
        "resumed" => RunPhase::Resumed,
        "rejected" => RunPhase::Rejected,
        _ => RunPhase::Queued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_pipeline::subject_summary::{StageSummaryData, SubjectSummaryData};

    #[test]
    fn stage_state_covers_the_four_names() {
        assert_eq!(stage_state("approved"), StageState::Approved);
        assert_eq!(stage_state("outputs"), StageState::Outputs);
        assert_eq!(stage_state("stub"), StageState::Stub);
        assert_eq!(stage_state("missing"), StageState::Missing);
        assert_eq!(stage_state("nonsense"), StageState::Missing);
    }

    #[test]
    fn run_phase_maps_lifecycle_names() {
        assert_eq!(run_phase("queued"), RunPhase::Queued);
        assert_eq!(run_phase("running"), RunPhase::Running);
        assert_eq!(run_phase("awaiting_qc"), RunPhase::AwaitingQc);
        assert_eq!(run_phase("done"), RunPhase::Done);
        assert_eq!(run_phase("failed"), RunPhase::Failed);
        assert_eq!(run_phase("cancelled"), RunPhase::Cancelled);
        assert_eq!(run_phase("resumed"), RunPhase::Resumed);
        assert_eq!(run_phase("rejected"), RunPhase::Rejected);
    }

    #[test]
    fn unknown_stage_rows_are_skipped_not_relabelled() {
        let root = tempfile::tempdir().unwrap();
        let data = SubjectSummaryData {
            subj_id: "1".to_owned(),
            anchor_modality: "T1w".to_owned(),
            modalities: vec![],
            postop_modalities: vec![],
            stages: vec![
                StageSummaryData {
                    stage: "nonsense".to_owned(),
                    state: "outputs".to_owned(),
                    result: None,
                },
                StageSummaryData {
                    stage: "normalize".to_owned(),
                    state: "approved".to_owned(),
                    result: None,
                },
            ],
            checkreg_manifest: None,
            scene_manifest: None,
        };
        let dto = subject_summary(&data, root.path());
        assert_eq!(dto.stages.len(), 1);
        assert_eq!(dto.stages[0].stage, Stage::Normalize);
    }

    #[test]
    fn subject_summary_rewrites_manifest_paths_to_keys() {
        let root = tempfile::tempdir().unwrap();
        let ds = root.path();
        std::fs::create_dir_all(ds.join("derivatives/leaddbs/sub-1/locks")).unwrap();
        let manifest = ds.join("derivatives/leaddbs/sub-1/locks/checkreg_manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        let data = SubjectSummaryData {
            subj_id: "1".to_owned(),
            anchor_modality: "T1w".to_owned(),
            modalities: vec!["T1w".to_owned()],
            postop_modalities: vec!["CT".to_owned()],
            stages: vec![StageSummaryData {
                stage: "preprocess".to_owned(),
                state: "outputs".to_owned(),
                result: None,
            }],
            checkreg_manifest: Some(manifest.to_string_lossy().into_owned()),
            scene_manifest: None,
        };
        let dto = subject_summary(&data, ds);
        assert_eq!(dto.subj_id, "1");
        assert_eq!(dto.stages.len(), 1);
        assert_eq!(dto.stages[0].stage, Stage::Preprocess);
        assert_eq!(dto.stages[0].state, StageState::Outputs);
        assert_eq!(
            dto.checkreg_manifest.as_deref(),
            Some("derivatives/leaddbs/sub-1/locks/checkreg_manifest.json")
        );
    }

    #[test]
    fn qc_manifest_maps_slides_and_approval() {
        let root = tempfile::tempdir().unwrap();
        let ds = root.path();
        let moving = ds.join("derivatives/leaddbs/sub-1/coregistration/anat/m.nii.gz");
        std::fs::create_dir_all(moving.parent().unwrap()).unwrap();
        std::fs::write(&moving, b"x").unwrap();
        let log =
            ds.join("derivatives/leaddbs/sub-1/coregistration/log/sub-1_desc-coregmethod.json");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, br#"{"approval": true}"#).unwrap();
        let manifest = serde_json::json!({
            "slides": [{
                "checkreg_png": ds.join("derivatives/leaddbs/sub-1/locks/c.png").to_string_lossy(),
                "moving": moving.to_string_lossy(),
                "fixed": moving.to_string_lossy(),
                "branch": "coreg",
                "modality": "CT",
                "method_log": log.to_string_lossy(),
            }]
        });
        let qc = qc_manifest(&manifest, ds);
        assert_eq!(qc.slides.len(), 1);
        assert_eq!(qc.slides[0].index, 0);
        assert_eq!(qc.slides[0].modality, "CT");
        assert!(qc.slides[0].approved);
        assert_eq!(
            qc.slides[0].moving,
            "derivatives/leaddbs/sub-1/coregistration/anat/m.nii.gz"
        );
    }

    #[test]
    fn qc_manifest_never_leaks_an_absolute_out_of_root_path() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.nii.gz");
        std::fs::write(&secret, b"x").unwrap();
        let manifest = serde_json::json!({
            "slides": [{
                "checkreg_png": secret.to_string_lossy(),
                "moving": secret.to_string_lossy(),
                "fixed": secret.to_string_lossy(),
                "branch": "coreg",
                "modality": "CT",
                "method_log": "",
            }]
        });
        let qc = qc_manifest(&manifest, root.path());
        assert_eq!(qc.slides[0].moving, "");
        assert_eq!(qc.slides[0].fixed, "");
        assert_eq!(qc.slides[0].checkreg_png, "");
    }

    #[test]
    fn run_status_computes_elapsed() {
        let job = JobRow {
            id: "r".to_owned(),
            dataset: "demo".to_owned(),
            subject: "1".to_owned(),
            options: "{}".to_owned(),
            phase: "done".to_owned(),
            priority: 1,
            resume: false,
            gated_stage: None,
            stage: Some("reconstruct".to_owned()),
            stages_ok: 3,
            stages_failed: 0,
            stages_skipped: 1,
            message: None,
            client: None,
            created_at: 1000,
            started_at: Some(2000),
            finished_at: Some(4500),
            resumed_by: Some("next".to_owned()),
        };
        let s = run_status(&job);
        assert_eq!(s.phase, RunPhase::Done);
        assert_eq!(s.elapsed_ms, Some(2500));
        assert_eq!(s.stages_ok, 3);
        assert_eq!(s.resumed_by.as_deref(), Some("next"));
        assert!(s.started_at.unwrap().starts_with("1970-01-01T"));
    }
}
