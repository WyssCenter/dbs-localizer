// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Several subjects, one after another (reference behaviour `specs/13` §9–§11).
//!
//! Each subject runs every selected stage to its end before the next subject starts. A failure
//! in one subject is recorded and, with [`ErrorPolicy::Continue`], the batch moves on. With
//! [`ReviewPolicy::Defer`] a registration that waits for review ends that subject as
//! [`BatchPhase::AwaitingQc`] and never blocks the batch.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub use dbsloc_protocol::protocol::BatchPhase;

use crate::checkreg::ReviewSetup;
use crate::options::RunOptions;
use crate::protocol::RunEvent;
use crate::stages::Stage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub subject: String,
}

impl BatchItem {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorPolicy {
    #[default]
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewPolicy {
    /// The registration check runs as in a single-subject run: the viewer (or the front end's
    /// review hook) waits for a verdict, per subject.
    #[default]
    Interactive,
    /// Headless gate: an unreviewed registration ends the subject as awaiting review.
    Defer,
    /// No registration check (`checkreg = false`).
    Off,
}

#[derive(Clone, Default)]
pub struct BatchPolicy {
    pub on_error: ErrorPolicy,
    pub review: ReviewPolicy,
    /// In-process review hook and PNG writer (GUI); `None` uses the gate of `RunOptions`.
    pub review_setup: Option<ReviewSetup>,
}

#[derive(Debug, Clone)]
pub struct BatchOutcome {
    pub subject: String,
    pub phase: BatchPhase,
    pub stages_ok: u32,
    pub stages_failed: u32,
    pub stages_skipped: u32,
    pub message: Option<String>,
    /// The stage whose registration waits for review ([`BatchPhase::AwaitingQc`]).
    pub awaiting_stage: Option<Stage>,
    pub elapsed: Duration,
    /// [`crate::RunPlan::to_json`] of the subject, when planning succeeded.
    pub plan: Option<Value>,
}

impl BatchOutcome {
    fn not_run(subject: &str, phase: BatchPhase, message: impl Into<String>) -> Self {
        Self {
            subject: subject.to_owned(),
            phase,
            stages_ok: 0,
            stages_failed: 0,
            stages_skipped: 0,
            message: Some(message.into()),
            awaiting_stage: None,
            elapsed: Duration::ZERO,
            plan: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum BatchEvent {
    /// `index` is 0-based; front ends show `index + 1` of `total`.
    SubjectStarted {
        index: usize,
        total: usize,
        subject: String,
    },
    Run(String, RunEvent),
    SubjectFinished(BatchOutcome),
}

#[derive(Debug, Clone, Default)]
pub struct BatchSummary {
    pub outcomes: Vec<BatchOutcome>,
    pub elapsed: Duration,
}

impl BatchSummary {
    pub fn count(&self, phase: BatchPhase) -> usize {
        self.outcomes.iter().filter(|o| o.phase == phase).count()
    }

    pub fn any_failure(&self) -> bool {
        self.outcomes.iter().any(|o| o.phase.is_failure())
    }

    pub fn awaiting_qc(&self) -> Vec<&str> {
        self.outcomes
            .iter()
            .filter(|o| o.phase == BatchPhase::AwaitingQc)
            .map(|o| o.subject.as_str())
            .collect()
    }

    pub fn totals_json(&self) -> Value {
        let sum = |f: fn(&BatchOutcome) -> u32| self.outcomes.iter().map(f).sum::<u32>();
        json!({
            "subjects": self.outcomes.len(),
            "done": self.count(BatchPhase::Done),
            "failed": self.count(BatchPhase::Failed),
            "awaiting_qc": self.count(BatchPhase::AwaitingQc),
            "cancelled": self.count(BatchPhase::Cancelled),
            "plan_error": self.count(BatchPhase::PlanError),
            "stages_ok": sum(|o| o.stages_ok),
            "stages_failed": sum(|o| o.stages_failed),
            "stages_skipped": sum(|o| o.stages_skipped),
            "seconds": (self.elapsed.as_secs_f64() * 1000.0).round() / 1000.0,
        })
    }
}

/// Subject id without the `sub-` prefix, trimmed.
pub fn normalize_subject_id(raw: &str) -> String {
    let t = raw.trim();
    t.strip_prefix("sub-").unwrap_or(t).to_owned()
}

/// One subject id per line; blank lines and lines starting with `#` are ignored.
pub fn parse_subject_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(normalize_subject_id)
        .filter(|s| !s.is_empty())
        .collect()
}

/// The subjects of a batch in order: `explicit` as given (de-duplicated, first occurrence
/// kept), or with `all` every subject of the dataset in list order (`bids::list_subject_ids_in`).
pub fn expand_subjects(
    dataset_root: &Path,
    output_dir: Option<&Path>,
    explicit: &[String],
    all: bool,
) -> Vec<String> {
    let ids: Vec<String> = if all {
        crate::bids::list_subject_ids_in(dataset_root, output_dir)
    } else {
        explicit.iter().map(|s| normalize_subject_id(s)).collect()
    };
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}

/// `RunOptions` of one subject: the shared template with the subject and the review policy.
pub fn options_for(template: &RunOptions, subject: &str, review: ReviewPolicy) -> RunOptions {
    let mut o = template.clone();
    o.subject = normalize_subject_id(subject);
    match review {
        ReviewPolicy::Interactive => {}
        ReviewPolicy::Defer => {
            o.checkreg = true;
            o.headless = true;
        }
        ReviewPolicy::Off => o.checkreg = false,
    }
    o
}

/// The stages after `gated` in `stages`: what a subject still has to run once the registration
/// that stopped it is approved. An unknown `gated` keeps every stage.
pub fn remaining_stages(stages: &[Stage], gated: Stage) -> Vec<Stage> {
    match stages.iter().position(|s| *s == gated) {
        Some(i) => stages[i + 1..].to_vec(),
        None => stages.to_vec(),
    }
}

const CANCELLED_MESSAGE: &str = "cancelled by user";

/// Folds one subject's [`RunEvent`]s into its [`BatchOutcome`].
#[derive(Debug, Default, Clone)]
pub struct SubjectTally {
    ok: u32,
    failed: u32,
    skipped: u32,
    gate_failed: u32,
    awaiting: Option<String>,
    gate_message: Option<String>,
    first_failure: Option<String>,
    cancelled: bool,
    completed: bool,
}

impl SubjectTally {
    pub fn on_event(&mut self, ev: &RunEvent) {
        match ev {
            RunEvent::StageCompleted { .. } => self.ok += 1,
            RunEvent::StageSkipped { .. } => self.skipped += 1,
            RunEvent::StageAwaitingQc { stage } => self.awaiting = Some(stage.clone()),
            RunEvent::StageFailed { stage, message } => {
                self.failed += 1;
                if message == CANCELLED_MESSAGE {
                    self.cancelled = true;
                }
                if self.awaiting.as_deref() == Some(stage.as_str()) && self.gate_message.is_none() {
                    self.gate_failed += 1;
                    self.gate_message = Some(format!("{stage}: {message}"));
                } else if self.first_failure.is_none() {
                    self.first_failure = Some(format!("{stage}: {message}"));
                }
            }
            RunEvent::PipelineCompleted { .. } => self.completed = true,
            RunEvent::StageStarted { .. } | RunEvent::LogLine { .. } => {}
        }
    }

    pub fn awaiting_stage(&self) -> Option<Stage> {
        self.awaiting.as_deref().and_then(Stage::parse)
    }

    pub fn outcome(
        &self,
        subject: &str,
        review: ReviewPolicy,
        cancel_requested: bool,
        elapsed: Duration,
    ) -> BatchOutcome {
        let real_failed = self.failed - self.gate_failed;
        let defer = review == ReviewPolicy::Defer;
        let (phase, message) = if self.cancelled || cancel_requested {
            (BatchPhase::Cancelled, Some(CANCELLED_MESSAGE.to_owned()))
        } else if real_failed > 0 {
            (BatchPhase::Failed, self.first_failure.clone())
        } else if self.awaiting.is_some() && defer {
            (
                BatchPhase::AwaitingQc,
                Some(format!(
                    "{} awaits review",
                    self.awaiting_stage().map_or_else(
                        || self.awaiting.clone().unwrap_or_default(),
                        |s| s.cli_name().to_owned()
                    )
                )),
            )
        } else if self.awaiting.is_some() {
            (BatchPhase::Failed, self.gate_message.clone())
        } else if !self.completed {
            (
                BatchPhase::Failed,
                Some("the pipeline stopped without reporting the end of the run".to_owned()),
            )
        } else {
            (BatchPhase::Done, None)
        };
        let stages_failed = if phase == BatchPhase::AwaitingQc {
            real_failed
        } else {
            self.failed
        };
        BatchOutcome {
            subject: subject.to_owned(),
            phase,
            stages_ok: self.ok,
            stages_failed,
            stages_skipped: self.skipped,
            message,
            awaiting_stage: (phase == BatchPhase::AwaitingQc)
                .then(|| self.awaiting_stage())
                .flatten(),
            elapsed,
            plan: None,
        }
    }
}

/// Runs one subject in process: `(options, events, cancel) -> failed stage count`.
pub type SubjectRunner<'a> =
    dyn Fn(&RunOptions, &mpsc::Sender<RunEvent>, Option<&AtomicBool>) -> u32 + Sync + 'a;

/// Run `subjects` one after another with the shared `template` options. Never panics on a
/// subject that cannot be planned: it becomes a [`BatchPhase::PlanError`] outcome. One `cancel`
/// stops the running subject and marks every later one [`BatchPhase::Cancelled`].
pub fn run_batch(
    template: &RunOptions,
    subjects: &[BatchItem],
    policy: &BatchPolicy,
    tx: &mpsc::Sender<BatchEvent>,
    cancel: Option<&AtomicBool>,
) -> BatchSummary {
    let setup = policy
        .review_setup
        .clone()
        .filter(|_| policy.review != ReviewPolicy::Off);
    let runner =
        move |o: &RunOptions, t: &mpsc::Sender<RunEvent>, c: Option<&AtomicBool>| match &setup {
            Some(s) => crate::run_pipeline_channel_with_review(o, t, c, s.clone()),
            None => crate::run_pipeline_channel(o, t, c),
        };
    run_batch_with(template, subjects, policy, tx, cancel, &runner)
}

pub fn run_batch_with(
    template: &RunOptions,
    subjects: &[BatchItem],
    policy: &BatchPolicy,
    tx: &mpsc::Sender<BatchEvent>,
    cancel: Option<&AtomicBool>,
    runner: &SubjectRunner<'_>,
) -> BatchSummary {
    let t0 = Instant::now();
    let total = subjects.len();
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::SeqCst));
    let mut outcomes: Vec<BatchOutcome> = Vec::with_capacity(total);
    let mut stop_reason: Option<String> = None;
    for (index, item) in subjects.iter().enumerate() {
        let subject = normalize_subject_id(&item.subject);
        if let Some(why) = &stop_reason {
            let outcome = BatchOutcome::not_run(&subject, BatchPhase::Cancelled, why.clone());
            let _ = tx.send(BatchEvent::SubjectFinished(outcome.clone()));
            outcomes.push(outcome);
            continue;
        }
        if cancelled() {
            stop_reason = Some(CANCELLED_MESSAGE.to_owned());
            let outcome = BatchOutcome::not_run(&subject, BatchPhase::Cancelled, CANCELLED_MESSAGE);
            let _ = tx.send(BatchEvent::SubjectFinished(outcome.clone()));
            outcomes.push(outcome);
            continue;
        }
        let _ = tx.send(BatchEvent::SubjectStarted {
            index,
            total,
            subject: subject.clone(),
        });
        let started = Instant::now();
        let opts = options_for(template, &subject, policy.review);
        let outcome = match crate::plan_run(&opts) {
            Err(e) => BatchOutcome {
                elapsed: started.elapsed(),
                ..BatchOutcome::not_run(&subject, BatchPhase::PlanError, e.to_string())
            },
            Ok(plan) => {
                let plan_json = plan.to_json(&opts);
                let mut tally = SubjectTally::default();
                let (etx, erx) = mpsc::channel::<RunEvent>();
                let panicked = std::thread::scope(|s| {
                    let opts_ref = &opts;
                    let handle = s.spawn(move || {
                        let failed = runner(opts_ref, &etx, cancel);
                        drop(etx);
                        failed
                    });
                    for ev in erx {
                        tally.on_event(&ev);
                        let _ = tx.send(BatchEvent::Run(subject.clone(), ev));
                    }
                    handle.join().is_err()
                });
                let mut outcome =
                    tally.outcome(&subject, policy.review, cancelled(), started.elapsed());
                if panicked && outcome.phase != BatchPhase::Cancelled {
                    outcome.phase = BatchPhase::Failed;
                    outcome.message = Some("the pipeline thread panicked".to_owned());
                }
                outcome.plan = Some(plan_json);
                outcome
            }
        };
        if outcome.phase == BatchPhase::Cancelled {
            stop_reason = Some(CANCELLED_MESSAGE.to_owned());
        } else if outcome.phase.is_failure() && policy.on_error == ErrorPolicy::Stop {
            stop_reason = Some(format!(
                "not run: the batch stopped after sub-{subject} failed"
            ));
        }
        let _ = tx.send(BatchEvent::SubjectFinished(outcome.clone()));
        outcomes.push(outcome);
    }
    BatchSummary {
        outcomes,
        elapsed: t0.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str, subjects: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_batch_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for s in subjects {
            let anat = d.join(format!("rawdata/sub-{s}/ses-preop/anat"));
            std::fs::create_dir_all(&anat).unwrap();
            std::fs::write(anat.join(format!("sub-{s}_ses-preop_T1w.nii.gz")), b"x").unwrap();
        }
        d
    }

    fn template(root: &Path) -> RunOptions {
        RunOptions {
            stages: vec![Stage::Preprocess, Stage::Coregister, Stage::Normalize],
            headless: true,
            no_scene3d: true,
            checkreg: true,
            stop_on_error: true,
            ..RunOptions::new(root, "")
        }
    }

    fn items(ids: &[&str]) -> Vec<BatchItem> {
        ids.iter().map(|s| BatchItem::new(*s)).collect()
    }

    fn send_stage(tx: &mpsc::Sender<RunEvent>, stage: Stage, ev: RunEvent) {
        let _ = tx.send(RunEvent::StageStarted {
            stage: stage.label().to_owned(),
        });
        let _ = tx.send(ev);
    }

    fn completed(stage: Stage) -> RunEvent {
        RunEvent::StageCompleted {
            stage: stage.label().to_owned(),
            result: Some("ok:fake".into()),
        }
    }

    fn finish(tx: &mpsc::Sender<RunEvent>, ok: u32, failed: u32) {
        let _ = tx.send(RunEvent::PipelineCompleted {
            stages_ok: ok,
            stages_failed: failed,
            stages_skipped: 0,
        });
    }

    /// Subject `2` fails in normalize; `q` stops at an unapproved coregistration gate; every
    /// other subject completes all its stages.
    fn fake(o: &RunOptions, tx: &mpsc::Sender<RunEvent>, _c: Option<&AtomicBool>) -> u32 {
        match o.subject.as_str() {
            "2" => {
                send_stage(tx, Stage::Preprocess, completed(Stage::Preprocess));
                send_stage(
                    tx,
                    Stage::Normalize,
                    RunEvent::StageFailed {
                        stage: Stage::Normalize.label().to_owned(),
                        message: "antsRegistration failed with 1".into(),
                    },
                );
                finish(tx, 1, 1);
                1
            }
            "q" => {
                send_stage(tx, Stage::Preprocess, completed(Stage::Preprocess));
                send_stage(
                    tx,
                    Stage::Coregister,
                    RunEvent::StageAwaitingQc {
                        stage: Stage::Coregister.label().to_owned(),
                    },
                );
                let _ = tx.send(RunEvent::StageFailed {
                    stage: Stage::Coregister.label().to_owned(),
                    message: "coreg: qc not approved".into(),
                });
                finish(tx, 1, 1);
                1
            }
            _ => {
                for st in &o.stages {
                    send_stage(tx, *st, completed(*st));
                }
                finish(tx, u32::try_from(o.stages.len()).unwrap(), 0);
                0
            }
        }
    }

    fn run(
        root: &Path,
        ids: &[&str],
        policy: &BatchPolicy,
        cancel: Option<&AtomicBool>,
        runner: &SubjectRunner<'_>,
    ) -> (BatchSummary, Vec<BatchEvent>) {
        let (tx, rx) = mpsc::channel();
        let summary = run_batch_with(&template(root), &items(ids), policy, &tx, cancel, runner);
        drop(tx);
        (summary, rx.into_iter().collect())
    }

    fn phases(s: &BatchSummary) -> Vec<BatchPhase> {
        s.outcomes.iter().map(|o| o.phase).collect()
    }

    #[test]
    fn a_failing_subject_warns_and_the_batch_continues() {
        let root = scratch("continue", &["1", "2", "3"]);
        let (s, events) = run(
            &root,
            &["1", "2", "3"],
            &BatchPolicy::default(),
            None,
            &fake,
        );
        assert_eq!(
            phases(&s),
            [BatchPhase::Done, BatchPhase::Failed, BatchPhase::Done]
        );
        assert_eq!(s.outcomes[0].stages_ok, 3);
        assert!(s.outcomes[1]
            .message
            .as_deref()
            .is_some_and(|m| m.contains("antsRegistration failed")));
        assert!(s.outcomes[0].plan.is_some());
        assert!(s.any_failure());
        let starts: Vec<(usize, usize, String)> = events
            .iter()
            .filter_map(|e| match e {
                BatchEvent::SubjectStarted {
                    index,
                    total,
                    subject,
                } => Some((*index, *total, subject.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            [(0, 3, "1".into()), (1, 3, "2".into()), (2, 3, "3".into())]
        );
        let first_of_3 = events
            .iter()
            .position(|e| matches!(e, BatchEvent::SubjectStarted { subject, .. } if subject == "3"))
            .unwrap();
        let end_of_2 = events
            .iter()
            .position(|e| matches!(e, BatchEvent::SubjectFinished(o) if o.subject == "2"))
            .unwrap();
        assert!(
            end_of_2 < first_of_3,
            "subject 3 starts after subject 2 ended"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_stop_policy_stops_after_the_first_failed_subject() {
        let root = scratch("stop", &["1", "2", "3"]);
        let policy = BatchPolicy {
            on_error: ErrorPolicy::Stop,
            ..BatchPolicy::default()
        };
        let (s, events) = run(&root, &["1", "2", "3"], &policy, None, &fake);
        assert_eq!(
            phases(&s),
            [BatchPhase::Done, BatchPhase::Failed, BatchPhase::Cancelled]
        );
        assert!(!events
            .iter()
            .any(|e| matches!(e, BatchEvent::SubjectStarted { subject, .. } if subject == "3")));
        assert!(s.outcomes[2]
            .message
            .as_deref()
            .is_some_and(|m| m.contains("sub-2 failed")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn defer_records_awaiting_review_and_moves_on_even_with_the_stop_policy() {
        let root = scratch("defer", &["1", "q", "3"]);
        let policy = BatchPolicy {
            on_error: ErrorPolicy::Stop,
            review: ReviewPolicy::Defer,
            review_setup: None,
        };
        let (s, _) = run(&root, &["1", "q", "3"], &policy, None, &fake);
        assert_eq!(
            phases(&s),
            [BatchPhase::Done, BatchPhase::AwaitingQc, BatchPhase::Done]
        );
        let q = &s.outcomes[1];
        assert_eq!(q.awaiting_stage, Some(Stage::Coregister));
        assert_eq!(q.stages_failed, 0);
        assert_eq!(s.awaiting_qc(), ["q"]);
        assert!(!s.any_failure());
        assert_eq!(s.totals_json()["awaiting_qc"], 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_interactive_rejection_is_a_failure() {
        let root = scratch("reject", &["q"]);
        let (s, _) = run(&root, &["q"], &BatchPolicy::default(), None, &fake);
        assert_eq!(phases(&s), [BatchPhase::Failed]);
        assert!(s.outcomes[0]
            .message
            .as_deref()
            .is_some_and(|m| m.contains("qc not approved")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn defer_and_off_set_the_gate_options() {
        let t = template(Path::new("/x"));
        let d = options_for(
            &RunOptions {
                headless: false,
                ..t.clone()
            },
            "sub-7",
            ReviewPolicy::Defer,
        );
        assert!(d.checkreg && d.headless);
        assert_eq!(d.subject, "7");
        assert!(!options_for(&t, "7", ReviewPolicy::Off).checkreg);
        let i = options_for(
            &RunOptions {
                headless: false,
                ..t
            },
            "7",
            ReviewPolicy::Interactive,
        );
        assert!(i.checkreg && !i.headless);
    }

    #[test]
    fn a_plan_error_does_not_stop_the_others() {
        let root = scratch("planerr", &["1", "3"]);
        let (s, _) = run(
            &root,
            &["1", "bogus", "3"],
            &BatchPolicy::default(),
            None,
            &fake,
        );
        assert_eq!(
            phases(&s),
            [BatchPhase::Done, BatchPhase::PlanError, BatchPhase::Done]
        );
        assert!(s.outcomes[1].message.is_some());
        assert!(s.outcomes[1].plan.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_mid_batch_stops_the_subject_and_skips_the_rest() {
        let root = scratch("cancel", &["1", "2", "3"]);
        let cancel = AtomicBool::new(false);
        let runner = |o: &RunOptions, tx: &mpsc::Sender<RunEvent>, c: Option<&AtomicBool>| {
            if o.subject == "2" {
                send_stage(tx, Stage::Preprocess, completed(Stage::Preprocess));
                if let Some(c) = c {
                    c.store(true, Ordering::SeqCst);
                }
                send_stage(
                    tx,
                    Stage::Coregister,
                    RunEvent::StageFailed {
                        stage: Stage::Coregister.label().to_owned(),
                        message: "cancelled by user".into(),
                    },
                );
                finish(tx, 1, 1);
                return 1;
            }
            fake(o, tx, c)
        };
        let (s, events) = run(
            &root,
            &["1", "2", "3"],
            &BatchPolicy::default(),
            Some(&cancel),
            &runner,
        );
        assert_eq!(
            phases(&s),
            [
                BatchPhase::Done,
                BatchPhase::Cancelled,
                BatchPhase::Cancelled
            ]
        );
        assert_eq!(s.outcomes[2].stages_ok, 0);
        assert!(!events
            .iter()
            .any(|e| matches!(e, BatchEvent::SubjectStarted { subject, .. } if subject == "3")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn subjects_expand_in_order_without_duplicates() {
        let root = scratch("expand", &["b", "a", "c"]);
        let explicit: Vec<String> = ["sub-c", "a", " c ", "a"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(expand_subjects(&root, None, &explicit, false), ["c", "a"]);
        assert_eq!(expand_subjects(&root, None, &[], true), ["a", "b", "c"]);
        assert_eq!(
            parse_subject_list("# cohort\n01\n\n sub-02 \n#03\n01\n"),
            ["01", "02", "01"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remaining_stages_follow_the_gate() {
        let st = [
            Stage::Preprocess,
            Stage::Coregister,
            Stage::Normalize,
            Stage::Reconstruct,
        ];
        assert_eq!(
            remaining_stages(&st, Stage::Coregister),
            [Stage::Normalize, Stage::Reconstruct]
        );
        assert!(remaining_stages(&st, Stage::Reconstruct).is_empty());
        assert_eq!(remaining_stages(&st, Stage::Vat), st);
    }

    #[test]
    fn a_runner_without_a_pipeline_end_is_a_failure() {
        let mut t = SubjectTally::default();
        t.on_event(&completed(Stage::Preprocess));
        let o = t.outcome("1", ReviewPolicy::Interactive, false, Duration::ZERO);
        assert_eq!(o.phase, BatchPhase::Failed);
    }
}
