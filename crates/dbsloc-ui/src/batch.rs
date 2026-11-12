// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Runs of several subjects: the checked subjects, the queue that runs them one after another
//! and the review queue of [`ReviewMode::AfterAll`] (reference behaviour `specs/13` §8–§11).
//!
//! Pure state: the front end spawns each subject's run, feeds the result back with
//! [`BatchQueue::finish_current`] and opens the reviews [`BatchQueue::next_review`] hands out.

use std::collections::VecDeque;

use dbsloc_protocol::protocol::BatchPhase;
use dbsloc_protocol::stage::Stage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReviewMode {
    /// The run waits for the verdict on each registration (per subject).
    #[default]
    DuringRun,
    /// A subject stops at its first unreviewed registration; the reviews open after the queue.
    AfterAll,
    Off,
}

impl ReviewMode {
    pub const ALL: [ReviewMode; 3] = [ReviewMode::DuringRun, ReviewMode::AfterAll, ReviewMode::Off];

    pub fn label(self) -> &'static str {
        match self {
            ReviewMode::DuringRun => "During the run",
            ReviewMode::AfterAll => "After all subjects",
            ReviewMode::Off => "Off",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ReviewMode::DuringRun => {
                "The run waits after Coregister and after Normalize until you approve or reject the registration."
            }
            ReviewMode::AfterAll => {
                "Runs of several subjects never wait: a subject stops at a registration that needs review and the next subject starts. The reviews open one after another at the end; approving one runs that subject's remaining steps. A run of one subject reviews during the run."
            }
            ReviewMode::Off => "No review; a finished registration can still be reviewed from its step card.",
        }
    }
}

/// The subjects ticked for the next run, remembered per dataset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubjectChecks {
    pub bids_dir: String,
    pub subjects: Vec<String>,
}

impl SubjectChecks {
    fn owned_by(&self, bids_dir: &str) -> bool {
        self.bids_dir == bids_dir.trim()
    }

    pub fn is_checked(&self, bids_dir: &str, subject: &str) -> bool {
        self.owned_by(bids_dir) && self.subjects.iter().any(|s| s == subject)
    }

    fn adopt(&mut self, bids_dir: &str) {
        if !self.owned_by(bids_dir) {
            self.bids_dir = bids_dir.trim().to_owned();
            self.subjects.clear();
        }
    }

    pub fn set(&mut self, bids_dir: &str, subject: &str, on: bool) {
        self.adopt(bids_dir);
        self.subjects.retain(|s| s != subject);
        if on {
            self.subjects.push(subject.to_owned());
        }
    }

    pub fn set_many(&mut self, bids_dir: &str, subjects: &[String], on: bool) {
        for s in subjects {
            self.set(bids_dir, s, on);
        }
    }

    /// The checked subjects that `listed` still contains, in list order.
    pub fn ordered(&self, bids_dir: &str, listed: &[String]) -> Vec<String> {
        if !self.owned_by(bids_dir) {
            return Vec::new();
        }
        listed
            .iter()
            .filter(|s| self.subjects.contains(s))
            .cloned()
            .collect()
    }
}

/// One subject's turn: `stages` `None` = the shared selection, judged against the subject's
/// own files when the turn comes; `Some` = exactly these (the rest after an approved review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRun {
    pub subject: String,
    pub stages: Option<Vec<Stage>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubjectResult {
    pub subject: String,
    pub phase: BatchPhase,
    pub ok: u32,
    pub failed: u32,
    pub skipped: u32,
    pub message: Option<String>,
    pub awaiting: Option<Stage>,
    pub seconds: f64,
    /// The registration was rejected in the review queue.
    pub rework: bool,
}

impl SubjectResult {
    pub fn not_run(subject: &str, phase: BatchPhase, message: &str) -> Self {
        Self {
            subject: subject.to_owned(),
            phase,
            ok: 0,
            failed: 0,
            skipped: 0,
            message: Some(message.to_owned()),
            awaiting: None,
            seconds: 0.0,
            rework: false,
        }
    }

    pub fn status_text(&self) -> &'static str {
        if self.rework {
            "needs rework"
        } else {
            self.phase.label()
        }
    }

    pub fn counts_text(&self) -> String {
        format!(
            "{} ok / {} failed / {} skipped",
            self.ok, self.failed, self.skipped
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub subject: String,
    pub stage: Stage,
    pub remaining: Vec<Stage>,
}

/// The stages after `gated`; every stage when `gated` is not among them.
pub fn remaining_after(stages: &[Stage], gated: Stage) -> Vec<Stage> {
    match stages.iter().position(|s| *s == gated) {
        Some(i) => stages[i + 1..].to_vec(),
        None => stages.to_vec(),
    }
}

pub const NOT_RUN_STOPPED: &str = "not run: the batch was stopped";

#[derive(Debug, Clone)]
pub struct BatchQueue {
    order: Vec<String>,
    pending: VecDeque<QueuedRun>,
    current: Option<QueuedRun>,
    current_stages: Vec<Stage>,
    results: Vec<SubjectResult>,
    reviews: VecDeque<ReviewItem>,
    reviewing: Option<ReviewItem>,
    stop: bool,
    mode: ReviewMode,
}

impl BatchQueue {
    pub fn new(subjects: &[String], mode: ReviewMode) -> Self {
        let mut order: Vec<String> = Vec::new();
        for s in subjects {
            if !order.contains(s) {
                order.push(s.clone());
            }
        }
        Self {
            pending: order
                .iter()
                .map(|s| QueuedRun {
                    subject: s.clone(),
                    stages: None,
                })
                .collect(),
            order,
            current: None,
            current_stages: Vec::new(),
            results: Vec::new(),
            reviews: VecDeque::new(),
            reviewing: None,
            stop: false,
            mode,
        }
    }

    pub fn mode(&self) -> ReviewMode {
        self.mode
    }

    pub fn total(&self) -> usize {
        self.order.len()
    }

    /// 1-based position of `subject` in the batch.
    pub fn position(&self, subject: &str) -> usize {
        self.order
            .iter()
            .position(|s| s == subject)
            .map_or(0, |i| i + 1)
    }

    pub fn current(&self) -> Option<&QueuedRun> {
        self.current.as_ref()
    }

    pub fn reviewing(&self) -> Option<&ReviewItem> {
        self.reviewing.as_ref()
    }

    pub fn reviews_waiting(&self) -> usize {
        self.reviews.len()
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn results(&self) -> &[SubjectResult] {
        &self.results
    }

    pub fn stop_requested(&self) -> bool {
        self.stop
    }

    /// The next subject to run, unless one is running or a stop was requested.
    pub fn start_next(&mut self) -> Option<QueuedRun> {
        if self.stop || self.current.is_some() {
            return None;
        }
        let next = self.pending.pop_front()?;
        self.current = Some(next.clone());
        self.current_stages.clear();
        Some(next)
    }

    /// The stages the running subject actually runs (after its own files were judged).
    pub fn set_planned(&mut self, stages: &[Stage]) {
        self.current_stages = stages.to_vec();
    }

    pub fn finish_current(&mut self, result: SubjectResult) {
        let Some(run) = self.current.take() else {
            return;
        };
        if result.phase == BatchPhase::AwaitingQc && self.mode == ReviewMode::AfterAll {
            if let Some(stage) = result.awaiting {
                self.reviews.push_back(ReviewItem {
                    subject: run.subject.clone(),
                    stage,
                    remaining: remaining_after(&self.current_stages, stage),
                });
            }
        }
        self.record(result);
        if self.stop {
            self.cancel_pending();
        }
    }

    fn record(&mut self, result: SubjectResult) {
        match self
            .results
            .iter_mut()
            .find(|r| r.subject == result.subject)
        {
            Some(slot) => *slot = result,
            None => self.results.push(result),
        }
    }

    /// Stop once the running subject has finished; the rest is reported as not run.
    pub fn stop_after_current(&mut self) {
        self.stop = true;
        if self.current.is_none() {
            self.cancel_pending();
        }
    }

    fn cancel_pending(&mut self) {
        for run in std::mem::take(&mut self.pending) {
            if !self.results.iter().any(|r| r.subject == run.subject) {
                self.results.push(SubjectResult::not_run(
                    &run.subject,
                    BatchPhase::Cancelled,
                    NOT_RUN_STOPPED,
                ));
            }
        }
    }

    /// Every queued subject has had its turn (or the batch was stopped).
    pub fn runs_finished(&self) -> bool {
        self.current.is_none() && (self.pending.is_empty() || self.stop)
    }

    /// The next review once every run has finished; it stays current until
    /// [`Self::review_done`] or [`Self::skip_review`].
    pub fn next_review(&mut self) -> Option<ReviewItem> {
        if !self.runs_finished() || self.reviewing.is_some() || self.stop {
            return None;
        }
        let item = self.reviews.pop_front()?;
        self.reviewing = Some(item.clone());
        Some(item)
    }

    /// The verdict on the current review: approval queues the subject's remaining steps.
    pub fn review_done(&mut self, subject: &str, stage: Stage, approved: bool) -> bool {
        let matches = self
            .reviewing
            .as_ref()
            .is_some_and(|r| r.subject == subject && r.stage == stage);
        if !matches {
            return false;
        }
        let Some(item) = self.reviewing.take() else {
            return false;
        };
        if let Some(r) = self.results.iter_mut().find(|r| r.subject == item.subject) {
            r.awaiting = None;
            if approved {
                r.phase = BatchPhase::Done;
                r.message = Some(format!("{} approved", item.stage.label()));
            } else {
                r.phase = BatchPhase::Failed;
                r.rework = true;
                r.message = Some(format!("{} rejected", item.stage.label()));
            }
        }
        if approved && !item.remaining.is_empty() {
            self.pending.push_back(QueuedRun {
                subject: item.subject,
                stages: Some(item.remaining),
            });
        }
        true
    }

    /// Leave the current review for later: the subject stays awaiting review.
    pub fn skip_review(&mut self, note: Option<&str>) {
        if let Some(item) = self.reviewing.take() {
            if let (Some(r), Some(note)) = (
                self.results.iter_mut().find(|r| r.subject == item.subject),
                note,
            ) {
                r.message = Some(note.to_owned());
            }
        }
    }

    /// Nothing left to run or review.
    pub fn is_finished(&self) -> bool {
        self.runs_finished() && self.reviewing.is_none() && (self.reviews.is_empty() || self.stop)
    }

    /// End the batch now: queued runs and reviews are dropped (their subjects keep the
    /// outcome they have).
    pub fn abandon(&mut self) {
        self.stop = true;
        self.cancel_pending();
        self.reviewing = None;
        self.reviews.clear();
    }

    /// `Subject 2/5 · sub-03`, or `… (remaining steps)` for a resumed subject.
    pub fn progress_label(&self) -> Option<String> {
        let run = self.current.as_ref()?;
        let resumed = if run.stages.is_some() {
            " (remaining steps)"
        } else {
            ""
        };
        Some(format!(
            "Subject {}/{} · {}{resumed}",
            self.position(&run.subject),
            self.total(),
            run.subject
        ))
    }

    pub fn count(&self, phase: BatchPhase) -> usize {
        self.results
            .iter()
            .filter(|r| r.phase == phase && !r.rework)
            .count()
    }

    pub fn rework_count(&self) -> usize {
        self.results.iter().filter(|r| r.rework).count()
    }

    pub fn summary_line(&self) -> String {
        let mut parts = vec![
            format!("{} done", self.count(BatchPhase::Done)),
            format!(
                "{} failed",
                self.count(BatchPhase::Failed) + self.count(BatchPhase::PlanError)
            ),
        ];
        for (n, what) in [
            (self.count(BatchPhase::AwaitingQc), "awaiting review"),
            (self.rework_count(), "needs rework"),
            (self.count(BatchPhase::Cancelled), "cancelled"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        format!("{} subjects: {}", self.results.len(), parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn result(subject: &str, phase: BatchPhase, awaiting: Option<Stage>) -> SubjectResult {
        SubjectResult {
            subject: subject.to_owned(),
            phase,
            ok: 1,
            failed: u32::from(phase == BatchPhase::Failed),
            skipped: 0,
            message: None,
            awaiting,
            seconds: 1.0,
            rework: false,
        }
    }

    const STAGES: [Stage; 3] = [Stage::Coregister, Stage::Normalize, Stage::Reconstruct];

    #[test]
    fn checks_follow_the_list_order_and_the_dataset() {
        let mut c = SubjectChecks::default();
        c.set("/a", "sub-03", true);
        c.set("/a", "sub-01", true);
        let listed = ids(&["sub-01", "sub-02", "sub-03"]);
        assert_eq!(c.ordered("/a", &listed), ["sub-01", "sub-03"]);
        assert!(c.is_checked("/a", "sub-03"));
        assert!(!c.is_checked("/b", "sub-03"));
        assert!(c.ordered("/b", &listed).is_empty());
        c.set("/a", "sub-03", false);
        assert_eq!(c.ordered("/a", &listed), ["sub-01"]);
        c.set_many("/a", &listed, true);
        assert_eq!(c.ordered("/a", &listed).len(), 3);
        c.set("/b", "sub-09", true);
        assert_eq!(c.subjects, ["sub-09"], "another dataset starts afresh");
        let text = serde_json::to_string(&c).expect("serialize");
        let back: SubjectChecks = serde_json::from_str(&text).expect("load");
        assert_eq!(back, c);
    }

    #[test]
    fn the_queue_runs_subjects_one_after_another() {
        let mut q = BatchQueue::new(&ids(&["sub-01", "sub-02", "sub-03"]), ReviewMode::DuringRun);
        let first = q.start_next().expect("first");
        assert_eq!(first.subject, "sub-01");
        assert!(q.start_next().is_none(), "one subject at a time");
        assert_eq!(q.progress_label().as_deref(), Some("Subject 1/3 · sub-01"));
        q.finish_current(result("sub-01", BatchPhase::Done, None));
        assert_eq!(q.start_next().map(|r| r.subject).as_deref(), Some("sub-02"));
        q.finish_current(result("sub-02", BatchPhase::Failed, None));
        assert_eq!(q.start_next().map(|r| r.subject).as_deref(), Some("sub-03"));
        q.finish_current(result("sub-03", BatchPhase::Done, None));
        assert!(q.start_next().is_none());
        assert!(q.is_finished());
        assert_eq!(q.results().len(), 3);
        assert!(q.summary_line().starts_with("3 subjects: 2 done, 1 failed"));
    }

    #[test]
    fn stop_after_this_subject_reports_the_rest_as_not_run() {
        let mut q = BatchQueue::new(&ids(&["sub-01", "sub-02", "sub-03"]), ReviewMode::DuringRun);
        q.start_next();
        q.stop_after_current();
        assert!(!q.is_finished(), "the running subject still has to end");
        q.finish_current(result("sub-01", BatchPhase::Done, None));
        assert!(q.start_next().is_none());
        assert!(q.is_finished());
        let phases: Vec<BatchPhase> = q.results().iter().map(|r| r.phase).collect();
        assert_eq!(
            phases,
            [
                BatchPhase::Done,
                BatchPhase::Cancelled,
                BatchPhase::Cancelled
            ]
        );
        assert_eq!(q.results()[2].message.as_deref(), Some(NOT_RUN_STOPPED));
    }

    #[test]
    fn reviews_open_in_order_after_the_queue_and_approval_requeues_the_rest() {
        let mut q = BatchQueue::new(&ids(&["sub-01", "sub-02", "sub-03"]), ReviewMode::AfterAll);
        for (s, awaiting) in [
            ("sub-01", Some(Stage::Normalize)),
            ("sub-02", None),
            ("sub-03", Some(Stage::Coregister)),
        ] {
            q.start_next();
            q.set_planned(&STAGES);
            assert!(q.next_review().is_none(), "no review while subjects run");
            let phase = if awaiting.is_some() {
                BatchPhase::AwaitingQc
            } else {
                BatchPhase::Done
            };
            q.finish_current(result(s, phase, awaiting));
        }
        assert_eq!(q.reviews_waiting(), 2);
        let first = q.next_review().expect("first review");
        assert_eq!(
            (first.subject.as_str(), first.stage),
            ("sub-01", Stage::Normalize)
        );
        assert_eq!(first.remaining, [Stage::Reconstruct]);
        assert!(q.next_review().is_none(), "one review at a time");
        assert!(
            !q.review_done("sub-03", Stage::Coregister, true),
            "not the open one"
        );
        assert!(q.review_done("sub-01", Stage::Normalize, true));
        let second = q.next_review();
        assert!(
            second.is_none(),
            "the approved subject's remaining steps run first"
        );
        let resumed = q.start_next().expect("resume");
        assert_eq!(resumed.stages.as_deref(), Some(&[Stage::Reconstruct][..]));
        assert_eq!(
            q.progress_label().as_deref(),
            Some("Subject 1/3 · sub-01 (remaining steps)")
        );
        q.set_planned(&[Stage::Reconstruct]);
        q.finish_current(result("sub-01", BatchPhase::Done, None));
        let second = q.next_review().expect("second review");
        assert_eq!(second.subject, "sub-03");
        assert!(q.review_done("sub-03", Stage::Coregister, false));
        assert!(q.start_next().is_none(), "a rejection queues nothing");
        assert!(q.is_finished());
        assert_eq!(q.results().len(), 3);
        let s3 = &q.results()[2];
        assert!(s3.rework && s3.phase == BatchPhase::Failed);
        assert_eq!(s3.status_text(), "needs rework");
        assert!(q.summary_line().contains("1 needs rework"));
    }

    #[test]
    fn during_run_mode_never_queues_reviews() {
        let mut q = BatchQueue::new(&ids(&["sub-01"]), ReviewMode::DuringRun);
        q.start_next();
        q.finish_current(result(
            "sub-01",
            BatchPhase::AwaitingQc,
            Some(Stage::Coregister),
        ));
        assert!(q.next_review().is_none());
        assert!(q.is_finished());
    }

    #[test]
    fn a_skipped_review_leaves_the_subject_awaiting() {
        let mut q = BatchQueue::new(&ids(&["sub-01"]), ReviewMode::AfterAll);
        q.start_next();
        q.set_planned(&STAGES);
        q.finish_current(result(
            "sub-01",
            BatchPhase::AwaitingQc,
            Some(Stage::Coregister),
        ));
        q.next_review().expect("review");
        q.skip_review(Some("the review images could not be prepared"));
        assert!(q.is_finished());
        assert_eq!(q.results()[0].phase, BatchPhase::AwaitingQc);
        assert_eq!(
            q.results()[0].message.as_deref(),
            Some("the review images could not be prepared")
        );
    }

    #[test]
    fn abandon_drops_runs_and_reviews() {
        let mut q = BatchQueue::new(&ids(&["sub-01", "sub-02"]), ReviewMode::AfterAll);
        q.start_next();
        q.set_planned(&STAGES);
        q.finish_current(result(
            "sub-01",
            BatchPhase::AwaitingQc,
            Some(Stage::Coregister),
        ));
        q.abandon();
        assert!(q.is_finished());
        assert!(q.next_review().is_none());
        assert_eq!(q.results()[1].phase, BatchPhase::Cancelled);
    }

    #[test]
    fn duplicates_are_queued_once() {
        let q = BatchQueue::new(&ids(&["sub-01", "sub-01", "sub-02"]), ReviewMode::Off);
        assert_eq!(q.total(), 2);
        assert_eq!(q.pending_len(), 2);
    }

    #[test]
    fn review_modes_serialize_by_name() {
        for m in ReviewMode::ALL {
            let text = serde_json::to_string(&m).expect("serialize");
            let back: ReviewMode = serde_json::from_str(&text).expect("load");
            assert_eq!(back, m);
            assert!(!m.label().is_empty() && !m.hint().is_empty());
        }
        assert_eq!(
            remaining_after(&STAGES, Stage::Vat),
            STAGES.to_vec(),
            "an unknown gate keeps every stage"
        );
    }
}
