// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pipeline stage enum and dispatch — ``specs/00_overview.md`` §5.1.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use crate::bids::Subject;
use crate::checkreg::{self, ReviewSetup};
use crate::error::{PipelineError, PipelineResult};
use crate::options::RunOptions;
use crate::prereqs::prereq_error;
use crate::protocol::{RunEvent, StageOutcome};
use crate::stage_impl;

pub use dbsloc_protocol::stage::Stage;

pub fn plan_outcome(stage: Stage) -> StageOutcome {
    StageOutcome::skipped_as("plan-only run", format!("plan:{}", stage.cli_name()))
}

pub fn run_stage(
    opts: &RunOptions,
    stage: Stage,
    log_lines: Option<&mpsc::Sender<RunEvent>>,
    cancel: Option<&AtomicBool>,
) -> PipelineResult<StageOutcome> {
    run_stage_with_review(opts, stage, log_lines, cancel, None)
}

pub fn run_stage_with_review(
    opts: &RunOptions,
    stage: Stage,
    log_lines: Option<&mpsc::Sender<RunEvent>>,
    cancel: Option<&AtomicBool>,
    review: Option<&ReviewSetup>,
) -> PipelineResult<StageOutcome> {
    if opts.plan_only {
        return Ok(plan_outcome(stage));
    }

    let sub = opts
        .resolve_subject()
        .map_err(|e| PipelineError::Bids(format!("subject resolution failed: {e}")))?;

    if let Some(msg) = prereq_error(&sub, opts, stage) {
        return Err(PipelineError::Message(msg));
    }

    let out = match stage {
        Stage::Bids => {
            return Ok(StageOutcome::skipped_as(
                "no stage body: the subject is resolved at run start (specs/01)",
                "plan:bids",
            ))
        }
        Stage::Precoreg => crate::precoreg::run_precoreg(&sub, opts, cancel, log_lines)?,
        Stage::Preprocess => stage_impl::run_preprocess(&sub, opts, cancel, log_lines)?,
        Stage::Coregister => {
            let out = stage_impl::run_coregister(&sub, opts, cancel, log_lines)?;
            note_methods(&sub, opts, stage, &[], &out, log_lines);
            if opts.checkreg {
                let sub = opts.resolve_subject()?;
                checkreg::run_coreg_checkreg(&sub, opts, review, cancel, log_lines)
                    .map_err(PipelineError::from)?;
            }
            return Ok(out);
        }
        Stage::Normalize => {
            let out = stage_impl::run_normalize(&sub, opts, cancel, log_lines)?;
            note_methods(&sub, opts, stage, &[], &out, log_lines);
            if opts.checkreg {
                let sub = opts.resolve_subject()?;
                checkreg::run_norm_checkreg(&sub, opts, review, cancel, log_lines)
                    .map_err(PipelineError::from)?;
            }
            return Ok(out);
        }
        Stage::Brainshift => stage_impl::run_brainshift(&sub, opts, review, cancel, log_lines)?,
        Stage::Reconstruct => stage_impl::run_reconstruct(&sub, opts, cancel, log_lines)?,
        Stage::Orientation => {
            crate::orientation_stage::run_orientation(&sub, opts, cancel, log_lines)?
        }
        Stage::Vat => {
            let mut computed: Vec<String> = Vec::new();
            let result = crate::vat::stage::run_vat_collecting(
                &sub,
                opts,
                &crate::vat::stage::VatParams::default(),
                cancel,
                log_lines,
                &mut computed,
            );
            note_methods_result(&sub, opts, stage, &computed, &result, log_lines);
            return result;
        }
        Stage::Visualize => stage_impl::run_visualize(&sub, opts, cancel, log_lines)?,
    };
    note_methods(&sub, opts, stage, &[], &out, log_lines);
    Ok(out)
}

pub(crate) fn note_methods(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    vat_labels: &[String],
    outcome: &StageOutcome,
    log_lines: Option<&mpsc::Sender<RunEvent>>,
) {
    if !matches!(outcome, StageOutcome::Completed(_)) {
        return;
    }
    if let Err(e) = crate::methods_text::record_stage(sub, opts, stage, vat_labels) {
        stage_impl::log(log_lines, format!("dbsloc: warning: methods text: {e}"));
    }
}

pub(crate) fn note_methods_result(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    vat_labels: &[String],
    result: &PipelineResult<StageOutcome>,
    log_lines: Option<&mpsc::Sender<RunEvent>>,
) {
    match result {
        Ok(outcome) => note_methods(sub, opts, stage, vat_labels, outcome, log_lines),
        Err(_) if stage == Stage::Vat => {
            if let Err(e) = crate::methods_text::record_stage(sub, opts, stage, vat_labels) {
                stage_impl::log(log_lines, format!("dbsloc: warning: methods text: {e}"));
            }
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::Stage;
    use crate::options::RunOptions;

    fn opts_plan_only() -> RunOptions {
        RunOptions {
            stages: vec![Stage::Preprocess],
            headless: true,
            no_scene3d: true,
            plan_only: true,
            stop_on_error: false,
            ..RunOptions::new("/tmp/bids", "01")
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_stages_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
        std::fs::write(
            d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
            b"x",
        )
        .unwrap();
        d
    }

    #[test]
    fn plan_only_reports_skipped_not_completed() {
        let opts = opts_plan_only();
        let result = super::run_stage(&opts, Stage::Preprocess, None, None);
        assert_eq!(
            result.ok(),
            Some(crate::protocol::StageOutcome::skipped_as(
                "plan-only run",
                "plan:preprocess"
            ))
        );
    }

    #[test]
    fn plan_only_and_skipped_outcomes_write_no_methods_text() {
        let root = scratch("skipped");
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let opts = RunOptions::new(&root, "1");
        let planned = super::plan_outcome(Stage::Preprocess);
        super::note_methods(&sub, &opts, Stage::Preprocess, &[], &planned, None);
        let skipped = crate::protocol::StageOutcome::skipped("nothing to do");
        super::note_methods(&sub, &opts, Stage::Preprocess, &[], &skipped, None);
        assert!(!crate::derivatives::methods_log(&sub).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_completed_outcome_appends_one_entry_and_a_write_failure_only_warns() {
        let root = scratch("completed");
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let opts = RunOptions::new(&root, "1");
        let done = crate::protocol::StageOutcome::done("ok:preprocess:n4:x");
        super::note_methods(&sub, &opts, Stage::Preprocess, &[], &done, None);
        let text = std::fs::read_to_string(crate::derivatives::methods_log(&sub)).unwrap();
        assert_eq!(
            text.matches("] preprocess | Wyss DBS Localizer ").count(),
            1
        );
        super::note_methods(&sub, &opts, Stage::Visualize, &[], &done, None);
        super::note_methods(&sub, &opts, Stage::Bids, &[], &done, None);
        assert_eq!(
            std::fs::read_to_string(crate::derivatives::methods_log(&sub)).unwrap(),
            text
        );

        let blocked = scratch("blocked");
        let sub = crate::bids::resolve_subject_with(&blocked, "1", false).unwrap();
        std::fs::create_dir_all(sub.log_dir.parent().unwrap()).unwrap();
        std::fs::write(&sub.log_dir, b"a file where the directory belongs").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        super::note_methods(&sub, &opts, Stage::Preprocess, &[], &done, Some(&tx));
        let line = match rx.try_recv() {
            Ok(crate::protocol::RunEvent::LogLine { line }) => line,
            other => panic!("{other:?}"),
        };
        assert!(
            line.starts_with("dbsloc: warning: methods text: "),
            "{line}"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&blocked);
    }

    #[test]
    fn a_failed_vat_stage_still_records_the_labels_computed_before_the_failure() {
        let root = scratch("vat_error");
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let opts = RunOptions::new(&root, "1");
        let dir = crate::vat::settings::stimulations_dir(&sub.subj_dir).join("native");
        let sidecar = dir
            .join("done")
            .join(format!("sub-{}_desc-vta.json", sub.subj_id));
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, r#"{"model": "kuncel"}"#).unwrap();
        let other = dir
            .join("other")
            .join(format!("sub-{}_desc-vta.json", sub.subj_id));
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, r#"{"model": "maedler"}"#).unwrap();
        let computed = vec!["done".to_owned()];
        let failed: crate::error::PipelineResult<crate::protocol::StageOutcome> = Err(
            crate::error::PipelineError::Message("vat: E_VAT_x: other".into()),
        );
        super::note_methods_result(&sub, &opts, Stage::Vat, &computed, &failed, None);
        let text = std::fs::read_to_string(crate::derivatives::methods_log(&sub)).unwrap();
        assert_eq!(text.matches("] vat | Wyss DBS Localizer ").count(), 1);
        assert!(text.contains("Kuncel et al. (2008)"), "{text}");
        assert!(!text.contains("Mädler"), "{text}");
        assert!(text.contains("1 stimulation setting "), "{text}");
        super::note_methods_result(&sub, &opts, Stage::Coregister, &computed, &failed, None);
        super::note_methods_result(&sub, &opts, Stage::Preprocess, &computed, &failed, None);
        assert_eq!(
            std::fs::read_to_string(crate::derivatives::methods_log(&sub)).unwrap(),
            text
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn names_round_trip_and_coreg_is_an_alias() {
        for s in Stage::default_sequence() {
            assert!(!s.label().is_empty());
            assert_eq!(Stage::parse(s.cli_name()), Some(*s));
            assert_eq!(Stage::parse(s.label()), Some(*s));
        }
        let s: Stage = serde_json::from_str("\"coreg\"").unwrap();
        assert_eq!(s, Stage::Coregister);
        let s: Stage = serde_json::from_str("\"coregister\"").unwrap();
        assert_eq!(s, Stage::Coregister);
        assert_eq!(Stage::parse("nonsense"), None);
        let names: Vec<&str> = Stage::default_sequence()
            .iter()
            .map(|s| s.cli_name())
            .collect();
        assert_eq!(
            names,
            [
                "bids",
                "preprocess",
                "precoreg",
                "coreg",
                "normalize",
                "brainshift",
                "reconstruct",
                "orientation",
                "vat",
                "visualize"
            ]
        );
    }
}
