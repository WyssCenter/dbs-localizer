// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use dbsloc_pipeline::stages::Stage;
use dbsloc_pipeline::{
    apply_decisions, slide_approved, slide_rejected, PngWriter, ReviewHook, ReviewOutcome,
    ReviewRequest,
};
use dbsloc_viz::checkreg_app::{CheckregReviewPanel, ReviewMode};
use dbsloc_viz::checkreg_view::ReviewDecision;
use eframe::egui;

use crate::panels::{callout, em, palette, popout_window, PopoutBar, STAGE_META};

pub const REVIEW_CLOSED: &str = "the review window closed";

const VERDICT_POLL: Duration = Duration::from_millis(100);

const POPOUT_SALT: &str = "dbsloc_review_popout";
const POPOUT_TITLE: &str = "Registration review - Wyss DBS Localizer";
const POPOUT_NAME: &str = "the registration review";

pub struct BridgeMsg {
    pub request: ReviewRequest,
    pub verdict: mpsc::Sender<ReviewOutcome>,
}

pub fn await_verdict(
    request: &ReviewRequest,
    cancel: Option<&AtomicBool>,
    to_ui: &mpsc::Sender<BridgeMsg>,
    notify: &dyn Fn(),
) -> ReviewOutcome {
    let (verdict_tx, verdict_rx) = mpsc::channel();
    let msg = BridgeMsg {
        request: request.clone(),
        verdict: verdict_tx,
    };
    if to_ui.send(msg).is_err() {
        return ReviewOutcome::Failed(REVIEW_CLOSED.to_owned());
    }
    notify();
    loop {
        match verdict_rx.recv_timeout(VERDICT_POLL) {
            Ok(outcome) => return outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                    return ReviewOutcome::Cancelled;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return ReviewOutcome::Failed(REVIEW_CLOSED.to_owned());
            }
        }
    }
}

pub fn make_hook(to_ui: mpsc::Sender<BridgeMsg>, ctx: egui::Context) -> Arc<ReviewHook> {
    Arc::new(move |request, cancel| {
        await_verdict(request, cancel, &to_ui, &|| ctx.request_repaint())
    })
}

/// The gate of a deferred review: it returns at once, so the method log (not yet approved)
/// stops the subject at this registration; the slides are written for the later review.
pub fn defer_setup() -> dbsloc_pipeline::ReviewSetup {
    dbsloc_pipeline::ReviewSetup {
        hook: Arc::new(|_request, _cancel| ReviewOutcome::Decided),
        png: PngWriter::default(),
    }
}

pub fn review_header(stage: Stage, subj_id: &str) -> String {
    let title = STAGE_META
        .iter()
        .find(|m| m.stage == stage)
        .map_or(stage.label(), |m| m.title);
    let subject = if subj_id.starts_with("sub-") {
        subj_id.to_owned()
    } else {
        format!("sub-{subj_id}")
    };
    format!("Registration review · {title} · {subject}")
}

pub fn request_from_manifest(path: &std::path::Path) -> Result<ReviewRequest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read manifest {}: {e}", path.display()))?;
    let manifest: dbsloc_viz::checkreg_app::CheckregManifest =
        serde_json::from_str(&text).map_err(|e| format!("bad manifest {}: {e}", path.display()))?;
    let first = manifest
        .slides
        .first()
        .ok_or_else(|| format!("manifest {} has no slides", path.display()))?;
    let (stage, branch) = if first.branch == "coreg" {
        (Stage::Coregister, "coreg")
    } else {
        (Stage::Normalize, "norm")
    };
    let method_log = first
        .method_log
        .clone()
        .unwrap_or_else(|| path.with_file_name("method_log.json"));
    let slides = manifest
        .slides
        .iter()
        .map(|s| dbsloc_pipeline::checkreg::SlideSpec {
            moving: s.moving.clone(),
            fixed: s.fixed.clone(),
            branch,
            modality: s.modality.clone().unwrap_or_default(),
            method_log: s.method_log.clone().unwrap_or_else(|| method_log.clone()),
            preproc: false,
        })
        .collect();
    Ok(ReviewRequest {
        stage,
        branch: branch.to_owned(),
        manifest: path.to_path_buf(),
        slides,
        subj_id: manifest.subj_id.clone(),
        method_log,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOwner {
    pub bids_dir: PathBuf,
    pub subject: String,
}

#[derive(Default)]
pub struct ReviewOutput {
    pub errors: Vec<String>,
    pub answered: Option<(Stage, bool)>,
    pub reviewed: Option<(Stage, bool)>,
    pub reviewed_owner: Option<ReviewOwner>,
    pub decided: bool,
}

impl ReviewOutput {
    pub fn merge(&mut self, other: ReviewOutput) {
        self.errors.extend(other.errors);
        if other.answered.is_some() {
            self.answered = other.answered;
        }
        if other.reviewed.is_some() {
            self.reviewed = other.reviewed;
            self.reviewed_owner = other.reviewed_owner;
        }
        self.decided |= other.decided;
    }
}

pub fn initial_slide_states(slides: &[(PathBuf, String, Option<String>)]) -> Vec<Option<bool>> {
    slides
        .iter()
        .map(|(log, branch, modality)| {
            if slide_approved(log, branch, modality.as_deref()) {
                Some(true)
            } else if !is_scalar_branch(branch) && slide_rejected(log, branch, modality.as_deref())
            {
                Some(false)
            } else {
                None
            }
        })
        .collect()
}

fn is_scalar_branch(branch: &str) -> bool {
    matches!(
        branch.to_lowercase().as_str(),
        "norm" | "normalization" | "brainshift"
    )
}

struct Session {
    inner: CheckregReviewPanel,
    request: ReviewRequest,
    verdict: Option<mpsc::Sender<ReviewOutcome>>,
    decisions: Vec<Option<bool>>,
    initial: Vec<Option<bool>>,
    error: Option<String>,
    after_run: bool,
    owner: ReviewOwner,
}

impl Session {
    fn slide_log(&self, slide: usize) -> Option<(PathBuf, String, Option<String>)> {
        let s = self.inner.manifest().slides.get(slide)?;
        let of_request = s.branch.eq_ignore_ascii_case(&self.request.branch);
        let log = if of_request && !self.request.method_log.as_os_str().is_empty() {
            self.request.method_log.clone()
        } else {
            s.method_log
                .clone()
                .unwrap_or_else(|| self.request.method_log.clone())
        };
        Some((log, s.branch.clone(), s.modality.clone()))
    }

    fn effective(&self) -> Vec<Option<bool>> {
        self.decisions
            .iter()
            .zip(&self.initial)
            .map(|(d, i)| d.or(*i))
            .collect()
    }

    fn refresh_states(&mut self) {
        let states = self.effective();
        self.inner.set_slide_states(states);
    }

    fn apply(&mut self, decision: ReviewDecision) -> Result<(), String> {
        let (log, branch, modality) = self
            .slide_log(decision.slide)
            .ok_or_else(|| format!("slide {} does not exist", decision.slide + 1))?;
        let Some(slot) = self.decisions.get_mut(decision.slide) else {
            return Err(format!("slide {} does not exist", decision.slide + 1));
        };
        let previous = *slot;
        *slot = Some(decision.approved());
        let to_write: Vec<(Option<String>, bool)> = if is_scalar_branch(&branch) {
            let siblings: Vec<(Option<String>, Option<bool>)> = (0..self.decisions.len())
                .filter_map(|i| {
                    let (l, b, m) = self.slide_log(i)?;
                    if l != log || !is_scalar_branch(&b) {
                        return None;
                    }
                    Some((m, self.decisions[i].or(self.initial[i])))
                })
                .collect();
            if siblings.iter().any(|(_, s)| *s == Some(false)) {
                vec![(None, false)]
            } else if siblings.iter().all(|(_, s)| *s == Some(true)) {
                vec![(None, true)]
            } else {
                Vec::new()
            }
        } else {
            vec![(modality, decision.approved())]
        };
        if let Err(e) = apply_decisions(&log, &branch, &to_write) {
            self.decisions[decision.slide] = previous;
            return Err(format!("could not record the decision in {e}"));
        }
        self.refresh_states();
        Ok(())
    }

    fn refresh_initial_states(&mut self) {
        let slides: Vec<_> = (0..self.decisions.len())
            .filter_map(|i| self.slide_log(i))
            .collect();
        self.initial = initial_slide_states(&slides);
        self.refresh_states();
    }

    fn complete(&self) -> bool {
        if self.decisions.is_empty() {
            return false;
        }
        if self.after_run {
            self.decisions.iter().any(Option::is_some)
                && self.effective().iter().all(Option::is_some)
        } else {
            self.decisions.iter().all(Option::is_some)
        }
    }

    fn all_approved(&self) -> bool {
        self.effective().iter().all(|s| *s == Some(true))
    }
}

#[derive(Default)]
pub struct ReviewPanel {
    session: Option<Session>,
    last_result: Option<String>,
    notice_error: Option<String>,
    popped_out: bool,
}

impl ReviewPanel {
    pub fn begin(&mut self, msg: BridgeMsg, owner: ReviewOwner) -> Result<(), String> {
        let BridgeMsg { request, verdict } = msg;
        let inner = match CheckregReviewPanel::from_manifest_path(&request.manifest) {
            Ok(panel) => panel,
            Err(e) => {
                let _ = verdict.send(ReviewOutcome::Failed(e.clone()));
                return Err(e);
            }
        };
        self.install(request, inner, Some(verdict), ReviewMode::DuringRun, owner);
        Ok(())
    }

    pub fn begin_after_run(
        &mut self,
        request: ReviewRequest,
        owner: ReviewOwner,
    ) -> Result<(), String> {
        let inner = CheckregReviewPanel::from_manifest_path(&request.manifest)?;
        self.install(request, inner, None, ReviewMode::AfterRun, owner);
        if let Some(session) = self.session.as_mut() {
            session.refresh_initial_states();
        }
        Ok(())
    }

    fn install(
        &mut self,
        request: ReviewRequest,
        mut inner: CheckregReviewPanel,
        verdict: Option<mpsc::Sender<ReviewOutcome>>,
        mode: ReviewMode,
        owner: ReviewOwner,
    ) {
        inner.set_mode(mode);
        let n = inner.slide_count();
        inner.set_slide_states(vec![None; n]);
        self.session = Some(Session {
            inner,
            request,
            verdict,
            decisions: vec![None; n],
            initial: vec![None; n],
            error: None,
            after_run: mode == ReviewMode::AfterRun,
            owner,
        });
        self.last_result = None;
        self.notice_error = None;
    }

    pub fn volumes_ready(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.inner.has_volumes() || s.inner.load_error().is_some())
    }

    pub fn session_stage(&self) -> Option<Stage> {
        self.session.as_ref().map(|s| s.request.stage)
    }

    pub fn set_notice_error(&mut self, message: Option<String>) {
        self.notice_error = message;
    }

    pub fn end_run(&mut self) {
        self.session = None;
        self.popped_out = false;
    }

    pub fn pop_out(&mut self) {
        self.popped_out = self.session.is_some();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> ReviewOutput {
        let mut out = ReviewOutput::default();
        if self.session.is_none() {
            self.popped_out = false;
            self.placeholder(ui);
            return out;
        }
        if !self.popped_out
            && ui.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::NONE,
                    egui::Key::F11,
                ))
            })
        {
            self.popped_out = true;
            ctx.request_repaint();
        }
        if self.popped_out {
            self.popped_notice(ui);
            return out;
        }
        self.session_ui(ui, None, &mut out);
        self.finish(&mut out);
        out
    }

    pub fn show_popout(&mut self, ctx: &egui::Context) -> ReviewOutput {
        let mut out = ReviewOutput::default();
        if !self.popped_out {
            return out;
        }
        if self.session.is_none() {
            self.popped_out = false;
            ctx.request_repaint();
            return out;
        }
        let bring_back = popout_window(
            ctx,
            POPOUT_SALT,
            POPOUT_TITLE,
            POPOUT_NAME,
            true,
            |ui, bar| {
                self.session_ui(ui, Some(bar), &mut out);
                self.finish(&mut out);
            },
        );
        if bring_back {
            self.popped_out = false;
            ctx.request_repaint();
        }
        out
    }

    fn placeholder(&self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("No registration is waiting for review.")
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            if let Some(error) = &self.notice_error {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("⚠ {error}"))
                            .small()
                            .color(pal.warn),
                    )
                    .wrap()
                    .selectable(false),
                );
            }
            if let Some(result) = &self.last_result {
                ui.add(
                    egui::Label::new(egui::RichText::new(result).small().color(pal.text_muted))
                        .selectable(false),
                );
            }
            ui.add_space(unit * 0.4);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "A finished step can be reviewed from its step card in the Pipeline tab.",
                    )
                    .small()
                    .color(pal.text_muted),
                )
                .wrap()
                .selectable(false),
            );
        });
    }

    fn popped_notice(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("The registration review is open in a separate window.")
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            ui.add_space(unit * 0.5);
            if ui
                .button("Dock to main window")
                .on_hover_text(
                    "Close the separate window and show the registration review in the main window again",
                )
                .clicked()
            {
                self.popped_out = false;
                ui.ctx().request_repaint();
            }
        });
    }

    fn session_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut bar: Option<&mut PopoutBar>,
        out: &mut ReviewOutput,
    ) {
        let pal = palette(ui);
        let mut pop = false;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(review_header(
                        session.request.stage,
                        &session.request.subj_id,
                    ))
                    .strong(),
                )
                .selectable(false),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(if session.after_run {
                        "Decisions are saved as you make them."
                    } else {
                        "The run waits for your decision."
                    })
                    .small()
                    .color(pal.text_muted),
                )
                .selectable(false),
            );
            if let Some(bar) = bar.as_deref_mut() {
                bar.show(ui);
            }
            if bar.is_none()
                && ui
                    .button("Open review in separate window")
                    .on_hover_text(
                        "Show the registration review in a separate window that can be maximised or set to full screen (F11)",
                    )
                    .clicked()
            {
                pop = true;
            }
        });
        if let Some(err) = session.error.clone() {
            callout(ui, pal.err, "Decision not recorded", |ui| {
                ui.add(egui::Label::new(egui::RichText::new(err).monospace().small()).wrap());
            });
        }
        let decisions = session.inner.show(ui);
        for decision in decisions {
            match session.apply(decision) {
                Ok(()) => {
                    session.error = None;
                    out.decided = true;
                }
                Err(e) => {
                    session.error = Some(e);
                }
            }
        }
        if pop {
            self.popped_out = true;
            ui.ctx().request_repaint();
        }
    }

    fn finish(&mut self, out: &mut ReviewOutput) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if !session.complete() {
            return;
        }
        let approved = session.all_approved();
        let stage = session.request.stage;
        let after_run = session.after_run;
        let owner = session.owner.clone();
        if let Some(verdict) = &session.verdict {
            let _ = verdict.send(ReviewOutcome::Decided);
        }
        self.last_result = Some(format!(
            "{}: registration {}.",
            stage.label(),
            if approved { "approved" } else { "rejected" }
        ));
        self.session = None;
        if after_run {
            out.reviewed = Some((stage, approved));
            out.reviewed_owner = Some(owner);
        } else {
            out.answered = Some((stage, approved));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_pipeline::apply_approval;
    use dbsloc_pipeline::checkreg::SlideSpec;
    use dbsloc_viz::checkreg_app::{CheckregManifest, CheckregSlide};
    use dbsloc_viz::checkreg_view::ReviewAction;
    use std::thread;
    use std::time::Instant;

    fn request(stage: Stage) -> ReviewRequest {
        ReviewRequest {
            stage,
            branch: "coreg".to_owned(),
            manifest: PathBuf::from("manifest.json"),
            slides: Vec::<SlideSpec>::new(),
            subj_id: "01".to_owned(),
            method_log: PathBuf::from("log.json"),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_review_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn session(log: &std::path::Path, branch: &str, modalities: &[&str]) -> Session {
        let slides = modalities
            .iter()
            .map(|m| CheckregSlide {
                checkreg_png: PathBuf::from("a.png"),
                moving: PathBuf::from("m.nii"),
                fixed: PathBuf::from("f.nii"),
                branch: branch.to_owned(),
                modality: Some((*m).to_owned()),
                method_log: Some(log.to_path_buf()),
            })
            .collect();
        let manifest = CheckregManifest {
            manifest_version: 1,
            subj_id: "01".to_owned(),
            active: 0,
            slides,
            subj_dir: None,
        };
        let mut panel = ReviewPanel::default();
        let mut req = request(Stage::Coregister);
        req.branch = branch.to_owned();
        req.method_log = log.to_path_buf();
        panel.install(
            req,
            CheckregReviewPanel::from_manifest(manifest),
            None,
            ReviewMode::DuringRun,
            owner(),
        );
        panel.session.take().expect("session")
    }

    fn owner() -> ReviewOwner {
        ReviewOwner {
            bids_dir: PathBuf::from("bids"),
            subject: "01".to_owned(),
        }
    }

    fn decision(slide: usize, action: ReviewAction) -> ReviewDecision {
        ReviewDecision { slide, action }
    }

    #[test]
    fn a_manifest_becomes_a_request() {
        let dir = scratch("manifest");
        let path = dir.join("checkreg_manifest.json");
        let text = r#"{"manifest_version":1,"subj_id":"07","active":0,"slides":[{"checkreg_png":"a.png","moving":"m.nii","fixed":"f.nii","branch":"norm","modality":"T1w","method_log":"log.json"}]}"#;
        std::fs::write(&path, text).expect("write");
        let r = request_from_manifest(&path).expect("request");
        assert_eq!(r.stage, Stage::Normalize);
        assert_eq!(r.subj_id, "07");
        assert_eq!(r.slides.len(), 1);
        assert_eq!(r.method_log, PathBuf::from("log.json"));
        assert!(request_from_manifest(&dir.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_verdict_from_the_ui_answers_the_hook() {
        let (tx, rx) = mpsc::channel::<BridgeMsg>();
        let ui = thread::spawn(move || {
            let msg = rx.recv().expect("request reaches the ui");
            assert_eq!(msg.request.subj_id, "01");
            msg.verdict.send(ReviewOutcome::Decided).expect("verdict");
        });
        let outcome = await_verdict(&request(Stage::Coregister), None, &tx, &|| {});
        assert_eq!(outcome, ReviewOutcome::Decided);
        ui.join().expect("ui thread");
    }

    #[test]
    fn the_hook_notifies_the_ui_that_a_request_is_waiting() {
        let (tx, rx) = mpsc::channel::<BridgeMsg>();
        let notified = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&notified);
        let ui = thread::spawn(move || {
            let msg = rx.recv().expect("request");
            msg.verdict.send(ReviewOutcome::Decided).expect("verdict");
        });
        let _ = await_verdict(&request(Stage::Normalize), None, &tx, &move || {
            seen.store(true, Ordering::SeqCst);
        });
        ui.join().expect("ui thread");
        assert!(notified.load(Ordering::SeqCst));
    }

    #[test]
    fn a_cancel_flag_ends_the_wait_quickly() {
        let (tx, rx) = mpsc::channel::<BridgeMsg>();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let setter = thread::spawn(move || {
            let msg = rx.recv().expect("request");
            thread::sleep(Duration::from_millis(30));
            flag.store(true, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(500));
            drop(msg);
        });
        let start = Instant::now();
        let outcome = await_verdict(&request(Stage::Coregister), Some(&cancel), &tx, &|| {});
        let waited = start.elapsed();
        assert_eq!(outcome, ReviewOutcome::Cancelled);
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
        setter.join().expect("setter thread");
    }

    #[test]
    fn a_dropped_receiver_fails_the_gate() {
        let (tx, rx) = mpsc::channel::<BridgeMsg>();
        drop(rx);
        let outcome = await_verdict(&request(Stage::Coregister), None, &tx, &|| {});
        assert_eq!(outcome, ReviewOutcome::Failed(REVIEW_CLOSED.to_owned()));
    }

    #[test]
    fn a_dropped_verdict_sender_fails_the_gate() {
        let (tx, rx) = mpsc::channel::<BridgeMsg>();
        let ui = thread::spawn(move || {
            let msg = rx.recv().expect("request");
            drop(msg);
        });
        let outcome = await_verdict(&request(Stage::Coregister), None, &tx, &|| {});
        assert_eq!(outcome, ReviewOutcome::Failed(REVIEW_CLOSED.to_owned()));
        ui.join().expect("ui thread");
    }

    #[test]
    fn coreg_decisions_are_written_per_modality() {
        let dir = scratch("coreg");
        let log = dir.join("coreg.json");
        let mut s = session(&log, "coreg", &["CT", "T2w"]);
        assert!(!s.complete());
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        assert!(slide_approved(&log, "coreg", Some("CT")));
        assert!(!slide_approved(&log, "coreg", Some("T2w")));
        assert_eq!(s.inner.slide_states(), &[Some(true), None]);
        s.apply(decision(1, ReviewAction::Reject)).expect("reject");
        assert!(s.complete());
        assert!(!s.all_approved());
        assert_eq!(s.inner.slide_states(), &[Some(true), Some(false)]);
        assert!(!dbsloc_pipeline::checkreg::method_log_allows_continue(&log));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approving_every_slide_lets_the_gate_continue() {
        let dir = scratch("all");
        let log = dir.join("coreg.json");
        let mut s = session(&log, "coreg", &["CT", "T2w"]);
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        s.apply(decision(1, ReviewAction::Approve))
            .expect("approve");
        assert!(s.complete());
        assert!(s.all_approved());
        assert!(dbsloc_pipeline::checkreg::method_log_allows_continue(&log));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_norm_decision_writes_the_scalar_approval() {
        let dir = scratch("norm");
        let log = dir.join("norm.json");
        let mut s = session(&log, "norm", &["T1w"]);
        s.apply(decision(0, ReviewAction::Reject)).expect("reject");
        assert!(!slide_approved(&log, "norm", None));
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(slide_approved(&log, "norm", None));
        assert!(s.complete());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_norm_decisions_reject_and_stop_the_gate() {
        let dir = scratch("norm_mixed");
        let log = dir.join("norm.json");
        let mut s = session(&log, "norm", &["T1w", "T2w", "CT"]);
        s.apply(decision(0, ReviewAction::Reject)).expect("reject");
        s.apply(decision(1, ReviewAction::Approve))
            .expect("approve");
        s.apply(decision(2, ReviewAction::Approve))
            .expect("approve");
        assert!(s.complete());
        assert!(!s.all_approved());
        assert!(!dbsloc_pipeline::checkreg::method_log_allows_continue(&log));
        assert_eq!(
            s.inner.slide_states(),
            &[Some(false), Some(true), Some(true)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approving_every_norm_slide_writes_one() {
        let dir = scratch("norm_all");
        let log = dir.join("norm.json");
        let mut s = session(&log, "norm", &["T1w", "T2w"]);
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        s.apply(decision(1, ReviewAction::Approve))
            .expect("approve");
        assert!(s.complete());
        assert!(s.all_approved());
        assert!(dbsloc_pipeline::checkreg::method_log_allows_continue(&log));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_after_run_decision_keeps_the_other_slides_marks() {
        let dir = scratch("after_marks");
        let log = dir.join("coreg.json");
        apply_approval(&log, "coreg", Some("CT"), true).expect("approve");
        apply_approval(&log, "coreg", Some("T2w"), false).expect("reject");
        let mut s = session(&log, "coreg", &["CT", "T2w"]);
        s.after_run = true;
        s.refresh_initial_states();
        assert_eq!(s.inner.slide_states(), &[Some(true), Some(false)]);
        assert!(!s.complete());
        s.apply(decision(1, ReviewAction::Reject)).expect("reject");
        assert_eq!(s.inner.slide_states(), &[Some(true), Some(false)]);
        assert!(s.complete());
        assert!(!s.all_approved());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slides_of_the_requested_stage_use_the_request_method_log() {
        let dir = scratch("request_log");
        let log = dir.join("coreg.json");
        let mut s = session(&dir.join("other.json"), "coreg", &["CT"]);
        s.request.method_log = log.clone();
        assert_eq!(s.slide_log(0).expect("slide").0, log);
        s.request.method_log = PathBuf::new();
        assert_eq!(s.slide_log(0).expect("slide").0, dir.join("other.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_header_names_the_step_and_the_subject() {
        assert_eq!(
            review_header(Stage::Coregister, "fixture"),
            "Registration review · Coregister post-operative image · sub-fixture"
        );
        assert_eq!(
            review_header(Stage::Normalize, "sub-07"),
            "Registration review · Normalize to template · sub-07"
        );
    }

    #[test]
    fn a_decision_for_a_missing_slide_is_refused() {
        let dir = scratch("missing");
        let mut s = session(&dir.join("coreg.json"), "coreg", &["CT"]);
        assert!(s.apply(decision(4, ReviewAction::Approve)).is_err());
        assert!(!s.complete());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finishing_answers_the_hook_and_clears_the_session() {
        let dir = scratch("finish");
        let log = dir.join("coreg.json");
        let (vtx, vrx) = mpsc::channel();
        let mut panel = ReviewPanel::default();
        let s = session(&log, "coreg", &["CT"]);
        panel.session = Some(Session {
            verdict: Some(vtx),
            ..s
        });
        let mut out = ReviewOutput::default();
        panel.finish(&mut out);
        assert!(out.answered.is_none(), "nothing decided yet");
        if let Some(s) = panel.session.as_mut() {
            s.apply(decision(0, ReviewAction::Approve))
                .expect("approve");
        }
        panel.finish(&mut out);
        assert_eq!(out.answered, Some((Stage::Coregister, true)));
        assert_eq!(vrx.recv().expect("verdict"), ReviewOutcome::Decided);
        assert!(panel.session.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ending_the_run_drops_the_pending_review() {
        let (vtx, vrx) = mpsc::channel();
        let mut panel = ReviewPanel::default();
        let s = session(&scratch("end").join("coreg.json"), "coreg", &["CT"]);
        panel.session = Some(Session {
            verdict: Some(vtx),
            ..s
        });
        panel.end_run();
        assert!(panel.session.is_none());
        assert!(vrx.recv().is_err());
    }

    #[test]
    fn a_stage_written_norm_log_starts_undecided_and_needs_every_slide() {
        let dir = scratch("stage_written");
        let log = dir.join("norm.json");
        std::fs::write(&log, r#"{"approval": 0}"#).expect("write");
        let mut s = session(&log, "norm", &["T1w", "T2w", "CT"]);
        s.after_run = true;
        s.refresh_initial_states();
        assert_eq!(s.inner.slide_states(), &[None, None, None]);
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        s.apply(decision(1, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        assert!(!dbsloc_pipeline::checkreg::method_log_allows_continue(&log));
        s.apply(decision(2, ReviewAction::Approve))
            .expect("approve");
        assert!(s.complete());
        assert!(s.all_approved());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(written["approval"], serde_json::json!(1));
        assert_eq!(written["reviewed"], serde_json::json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_coreg_review_leaves_the_unseen_slide_undecided() {
        let dir = scratch("coreg_cancel");
        let log = dir.join("coreg.json");
        std::fs::write(
            &log,
            r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": ["CT"]}"#,
        )
        .expect("write");
        assert!(!slide_rejected(&log, "coreg", Some("T2w")));
        let mut s = session(&log, "coreg", &["CT", "T2w"]);
        s.after_run = true;
        s.refresh_initial_states();
        assert_eq!(s.inner.slide_states(), &[Some(true), None]);
        assert!(!s.complete());
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        s.apply(decision(1, ReviewAction::Reject)).expect("reject");
        assert!(s.complete());
        assert!(slide_rejected(&log, "coreg", Some("T2w")));
        assert_eq!(
            dbsloc_pipeline::checkreg::review_state(&log),
            dbsloc_pipeline::checkreg::ReviewState::Rejected
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_legacy_coreg_marker_counts_every_modality_as_reviewed() {
        let dir = scratch("coreg_legacy");
        let log = dir.join("coreg.json");
        std::fs::write(
            &log,
            r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": true}"#,
        )
        .expect("write");
        let slides: Vec<_> = ["CT", "T2w"]
            .iter()
            .map(|m| (log.clone(), "coreg".to_owned(), Some((*m).to_owned())))
            .collect();
        assert_eq!(initial_slide_states(&slides), vec![Some(true), Some(false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rejected_norm_log_starts_every_slide_undecided_after_the_run() {
        let dir = scratch("norm_rejected");
        let log = dir.join("norm.json");
        std::fs::write(&log, r#"{"approval": 0, "reviewed": true}"#).expect("write");
        let mut s = session(&log, "norm", &["T1w", "T2w"]);
        s.after_run = true;
        s.refresh_initial_states();
        assert_eq!(s.inner.slide_states(), &[None, None]);
        s.apply(decision(0, ReviewAction::Approve))
            .expect("approve");
        assert!(!s.complete());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(written["approval"], serde_json::json!(0));
        s.apply(decision(1, ReviewAction::Approve))
            .expect("approve");
        assert!(s.complete());
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&log).expect("read")).expect("json");
        assert_eq!(written["approval"], serde_json::json!(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_approved_norm_log_starts_every_slide_approved() {
        let dir = scratch("norm_approved");
        let log = dir.join("norm.json");
        std::fs::write(&log, r#"{"approval": 1, "reviewed": true}"#).expect("write");
        let mut s = session(&log, "norm", &["T1w", "T2w"]);
        s.after_run = true;
        s.refresh_initial_states();
        assert_eq!(s.inner.slide_states(), &[Some(true), Some(true)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stage_written_coreg_log_starts_undecided() {
        let dir = scratch("stage_written_coreg");
        let log = dir.join("coreg.json");
        std::fs::write(&log, r#"{"approval": {"CT": 0, "T2w": 0}}"#).expect("write");
        let slides: Vec<_> = ["CT", "T2w"]
            .iter()
            .map(|m| (log.clone(), "coreg".to_owned(), Some((*m).to_owned())))
            .collect();
        assert_eq!(initial_slide_states(&slides), vec![None, None]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn initial_states_follow_the_method_log() {
        let dir = scratch("initial");
        let log = dir.join("coreg.json");
        let slides = |m: &[&str]| -> Vec<(PathBuf, String, Option<String>)> {
            m.iter()
                .map(|m| (log.clone(), "coreg".to_owned(), Some((*m).to_owned())))
                .collect()
        };
        assert_eq!(
            initial_slide_states(&slides(&["CT", "T2w"])),
            vec![None, None]
        );
        apply_approval(&log, "coreg", Some("CT"), true).expect("approve");
        apply_approval(&log, "coreg", Some("T2w"), false).expect("reject");
        assert_eq!(
            initial_slide_states(&slides(&["CT", "T2w"])),
            vec![Some(true), Some(false)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_after_the_fact_review_reports_through_reviewed_not_answered() {
        let dir = scratch("after");
        let log = dir.join("norm.json");
        let mut panel = ReviewPanel::default();
        let mut s = session(&log, "norm", &["T1w"]);
        s.after_run = true;
        panel.session = Some(s);
        assert!(panel.session_stage().is_some());
        assert_eq!(panel.session_stage(), Some(Stage::Coregister));
        if let Some(s) = panel.session.as_mut() {
            s.apply(decision(0, ReviewAction::Reject)).expect("reject");
        }
        let mut out = ReviewOutput::default();
        panel.finish(&mut out);
        assert!(out.answered.is_none());
        assert_eq!(out.reviewed, Some((Stage::Coregister, false)));
        assert!(panel.session_stage().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_outputs_keeps_the_latest_answers_and_every_error() {
        let mut a = ReviewOutput {
            errors: vec!["one".to_owned()],
            answered: Some((Stage::Coregister, true)),
            reviewed: None,
            reviewed_owner: None,
            decided: false,
        };
        a.merge(ReviewOutput {
            errors: vec!["two".to_owned()],
            answered: None,
            reviewed: Some((Stage::Normalize, false)),
            reviewed_owner: None,
            decided: true,
        });
        assert!(a.decided);
        assert_eq!(a.errors, vec!["one".to_owned(), "two".to_owned()]);
        assert_eq!(a.answered, Some((Stage::Coregister, true)));
        assert_eq!(a.reviewed, Some((Stage::Normalize, false)));
    }
}
