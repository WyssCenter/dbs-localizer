// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pipeline stage enum and dispatch — ``specs/00_overview.md`` §5.1.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

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

    match stage {
        Stage::Bids => Ok(StageOutcome::skipped_as(
            "no stage body: the subject is resolved at run start (specs/01)",
            "plan:bids",
        )),
        Stage::Precoreg => crate::precoreg::run_precoreg(&sub, opts, cancel, log_lines),
        Stage::Preprocess => stage_impl::run_preprocess(&sub, opts, cancel, log_lines),
        Stage::Coregister => {
            let out = stage_impl::run_coregister(&sub, opts, cancel, log_lines)?;
            if opts.checkreg {
                let sub = opts.resolve_subject()?;
                checkreg::run_coreg_checkreg(&sub, opts, review, cancel, log_lines)
                    .map_err(PipelineError::from)?;
            }
            Ok(out)
        }
        Stage::Normalize => {
            let out = stage_impl::run_normalize(&sub, opts, cancel, log_lines)?;
            if opts.checkreg {
                let sub = opts.resolve_subject()?;
                checkreg::run_norm_checkreg(&sub, opts, review, cancel, log_lines)
                    .map_err(PipelineError::from)?;
            }
            Ok(out)
        }
        Stage::Brainshift => stage_impl::run_brainshift(&sub, opts, review, cancel, log_lines),
        Stage::Reconstruct => stage_impl::run_reconstruct(&sub, opts, cancel, log_lines),
        Stage::Orientation => {
            crate::orientation_stage::run_orientation(&sub, opts, cancel, log_lines)
        }
        Stage::Vat => crate::vat::stage::run_vat(&sub, opts, cancel, log_lines),
        Stage::Visualize => stage_impl::run_visualize(&sub, opts, cancel, log_lines),
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
