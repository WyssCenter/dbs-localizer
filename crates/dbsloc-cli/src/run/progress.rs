// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Instant;

use dbsloc_pipeline::protocol::RunEvent;

use crate::localtime;

const LONG_RUN_STAGES: [&str; 3] = ["coreg", "normalize", "reconstruct"];
const ALWAYS_SHOWN: [&str; 2] = ["dbsloc: note:", "dbsloc: warning:"];

fn always_shown(line: &str) -> bool {
    ALWAYS_SHOWN.iter().any(|p| line.starts_with(p))
        || (line.starts_with("dbsloc: ")
            && (line.contains(" re-ran; approvals invalidated: ")
                || line.contains(" re-ran; reviews reset: ")))
}

pub fn label_for_stage(stage: &str) -> String {
    let fixed = match stage {
        "bids" => "BIDS layout",
        "preprocess" => "Preprocessing (N4)",
        "precoreg" => "Precoregistration",
        "coreg" => "Coregistration (post-op CT or MR \u{2192} anchor)",
        "normalize" => "Normalization (anchor \u{2192} template)",
        "brainshift" => "Brain shift",
        "reconstruct" => "Reconstruction",
        "orientation" => "Orientation",
        "vat" => "VAT (volume of activation)",
        "visualize" => "Visualization",
        other => return title_case(&other.replace('_', " ")),
    };
    fixed.to_owned()
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at_start = true;
    for c in s.chars() {
        if c.is_alphabetic() {
            if at_start {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            at_start = false;
        } else {
            out.push(c);
            at_start = true;
        }
    }
    out
}

fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn nth_field(s: &str, n: usize) -> &str {
    s.splitn(n + 1, ':').nth(n).unwrap_or("")
}

pub fn describe_stage_outcome(result: &str) -> String {
    let s = result;
    if s.starts_with("error:") {
        return s.to_owned();
    }
    if s.starts_with("skip:") && s.contains("approved") {
        return "Skipped: a lock file in locks/ says this stage was already approved with the same inputs."
            .to_owned();
    }
    if let Some(rest) = s.strip_prefix("plan:") {
        let rest = rest.trim();
        if rest.is_empty() {
            return "Not run in this invocation (plan-only).".to_owned();
        }
        return format!("Not executed here \u{2014} plan marker: {rest}.");
    }
    if !s.starts_with("ok:") {
        if s.chars().count() > 200 {
            let head: String = s.chars().take(200).collect();
            return format!("{head}\u{2026}");
        }
        return s.to_owned();
    }
    if s.starts_with("ok:coreg:") {
        return format!(
            "Done. Co-registered image {} and its transforms are under \u{2026}/coregistration/.",
            py_repr(nth_field(s, 2))
        );
    }
    if s.starts_with("ok:preprocess:n4:") {
        return format!(
            "Done. Bias-corrected image written as {} under \u{2026}/preprocessing/anat/.",
            py_repr(nth_field(s, 3))
        );
    }
    if let Some(tail) = s.strip_prefix("ok:normalization:ants_syn:") {
        let tail = tail.split('+').next().unwrap_or(tail);
        return format!(
            "Done. Anchor image in template space {} (with the template-space post-op images and warps) is under \u{2026}/normalization/.",
            py_repr(tail)
        );
    }
    if s.starts_with("ok:reconstruct:") {
        return format!(
            "Done. Reconstruction summary written as {} under \u{2026}/reconstruction/.",
            py_repr(nth_field(s, 2))
        );
    }
    if s.starts_with("ok:orientation:") {
        return format!(
            "Done. Orientation estimated for {} directional leads (resolved or ambiguous / directional); details in meta.orientation of the reconstruction file.",
            nth_field(s, 2)
        );
    }
    let core = &s[3..];
    if core.chars().count() < 90 {
        format!("Done ({core}).")
    } else {
        let head: String = core.chars().take(87).collect();
        format!("Done ({head}\u{2026}).")
    }
}

pub struct Friendly {
    subj_id: String,
    dataset_root: String,
    planned: Vec<String>,
    tick: &'static str,
    cross: &'static str,
    track_done: &'static str,
    track_pend: &'static str,
    n_done: usize,
}

impl Friendly {
    pub fn new(subj_id: &str, dataset_root: &Path, planned: &[String], plain: bool) -> Friendly {
        let tty = std::io::stderr().is_terminal();
        let ascii = plain || !tty;
        Friendly {
            subj_id: subj_id.to_owned(),
            dataset_root: dataset_root.display().to_string(),
            planned: planned.to_vec(),
            tick: if ascii { "[x]" } else { "\u{2713}" },
            cross: if ascii { "[!]" } else { "\u{2717}" },
            track_done: if tty && !plain { "\u{2713}" } else { "*" },
            track_pend: if tty && !plain { "\u{b7}" } else { "." },
            n_done: 0,
        }
    }

    pub fn header(&self) -> String {
        let n = self.planned.len();
        format!(
            "\nWyss DBS Localizer: Running pipeline for subject {} ({n} step{}).\nDataset: {}\n",
            self.subj_id,
            if n == 1 { "" } else { "s" },
            self.dataset_root
        )
    }

    pub fn before_stage(&self, stage: &str, ts: &str) -> String {
        let lab = label_for_stage(stage);
        let mut s = format!("  [{ts}]  Starting: {lab}\u{2026}");
        if LONG_RUN_STAGES.contains(&stage) {
            s.push_str("\n    May take several minutes.");
        }
        s
    }

    pub fn after_stage(&mut self, stage: &str, result: &str, seconds: f64, ts: &str) -> String {
        let lab = label_for_stage(stage);
        let is_err = result.starts_with("error:");
        let mark = if is_err { self.cross } else { self.tick };
        let sec = seconds.round_ties_even();
        let mut s = String::new();
        if is_err {
            s.push_str(&format!(
                "  [{ts}]  {mark}  {lab} \u{2014} failed after {sec}s.\n    Reason: {result}"
            ));
        } else {
            if result.starts_with("plan:") || result.starts_with("skip:") {
                let kind = if result.starts_with("skip:") {
                    "skipped"
                } else {
                    "plan only"
                };
                s.push_str(&format!(
                    "  [{ts}]  {mark}  {lab} \u{2014} {kind} ({sec}s)."
                ));
            } else {
                s.push_str(&format!(
                    "  [{ts}]  {mark}  {lab} \u{2014} completed successfully in {sec}s."
                ));
            }
            let detail = describe_stage_outcome(result);
            if !detail.is_empty() {
                s.push_str(&format!("\n    {detail}"));
            }
        }
        self.n_done += 1;
        if !self.planned.is_empty() && self.n_done <= self.planned.len() {
            let m: Vec<&str> = (0..self.planned.len())
                .map(|i| {
                    if i < self.n_done {
                        self.track_done
                    } else {
                        self.track_pend
                    }
                })
                .collect();
            s.push_str(&format!("\n  Progress: [{}]\n", m.join(" ")));
        }
        s
    }

    pub fn footer(
        &self,
        done: usize,
        failed: usize,
        interrupted: bool,
        total_seconds: f64,
        ts: &str,
    ) -> String {
        let n = self.planned.len();
        let head = if interrupted {
            format!("Pipeline interrupted after {done} of {n} step(s)")
        } else if done < n {
            format!("Pipeline stopped after {done} of {n} step(s)")
        } else {
            format!("Pipeline finished all {n} step(s)")
        };
        let tail = if failed > 0 {
            format!("; {failed} step(s) failed")
        } else {
            String::new()
        };
        format!("  [{ts}]  {head} in {total_seconds:.1}s total{tail}.\n")
    }
}

pub struct RunReporter {
    planned: Vec<String>,
    rpt: Option<Friendly>,
    show_log: bool,
    pub rows: Vec<(String, String, f64)>,
    t0: Instant,
}

fn emit(text: &str) {
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{text}");
    let _ = e.flush();
}

impl RunReporter {
    pub fn new(planned: &[String], rpt: Option<Friendly>, show_log: bool) -> RunReporter {
        RunReporter {
            planned: planned.to_vec(),
            rpt,
            show_log,
            rows: Vec::new(),
            t0: Instant::now(),
        }
    }

    pub fn header(&self) {
        if let Some(r) = &self.rpt {
            emit(&r.header());
        }
    }

    pub fn footer(&self, total_seconds: f64, interrupted: bool) {
        if let Some(r) = &self.rpt {
            let failed = self
                .rows
                .iter()
                .filter(|(_, res, _)| res.starts_with("error:"))
                .count();
            emit(&r.footer(
                self.rows.len(),
                failed,
                interrupted,
                total_seconds,
                &localtime::hms(),
            ));
        }
    }

    pub fn record_failure(&mut self, message: &str) {
        let result = format!("error: {message}");
        emit(&format!("dbsloc: {result}"));
        let name = self.name();
        self.rows
            .push((name, result, self.t0.elapsed().as_secs_f64()));
    }

    fn name(&self) -> String {
        self.planned
            .get(self.rows.len())
            .cloned()
            .unwrap_or_else(|| format!("stage{}", self.rows.len()))
    }

    pub fn handle(&mut self, ev: RunEvent) {
        let result = match ev {
            RunEvent::StageStarted { .. } => {
                self.t0 = Instant::now();
                if let Some(r) = &self.rpt {
                    emit(&r.before_stage(&self.name(), &localtime::hms()));
                }
                return;
            }
            RunEvent::LogLine { line } => {
                if self.show_log || always_shown(&line) {
                    emit(&line);
                }
                return;
            }
            RunEvent::StageCompleted { result, .. } => match result.filter(|r| !r.is_empty()) {
                Some(r) => r,
                None => format!("ok:{}", self.name()),
            },
            RunEvent::StageSkipped { reason, result, .. } => {
                match result.filter(|r| !r.is_empty()) {
                    Some(r) => r,
                    None => format!("skip:{reason}"),
                }
            }
            RunEvent::StageFailed { message, .. } => format!("error: {message}"),
            RunEvent::StageAwaitingQc { .. } | RunEvent::PipelineCompleted { .. } => return,
        };
        let dt = self.t0.elapsed().as_secs_f64();
        let name = self.name();
        if let Some(r) = &mut self.rpt {
            emit(&r.after_stage(&name, &result, dt, &localtime::hms()));
        }
        self.rows.push((name, result, dt));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_the_known_stages_and_title_case_the_rest() {
        assert_eq!(label_for_stage("vat"), "VAT (volume of activation)");
        assert_eq!(label_for_stage("some_new_stage"), "Some New Stage");
    }

    #[test]
    fn outcome_phrases() {
        assert!(describe_stage_outcome("skip:approved").starts_with("Skipped: a lock file"));
        assert_eq!(
            describe_stage_outcome("plan:vat"),
            "Not executed here \u{2014} plan marker: vat."
        );
        assert_eq!(
            describe_stage_outcome("plan:"),
            "Not run in this invocation (plan-only)."
        );
        assert!(describe_stage_outcome("ok:coreg:sub-1_x")
            .contains("image 'sub-1_x' and its transforms"));
        assert!(
            describe_stage_outcome("ok:preprocess:n4:a_desc-preproc_T1w.nii.gz")
                .contains("written as 'a_desc-preproc_T1w.nii.gz'")
        );
        assert!(describe_stage_outcome("ok:normalization:ants_syn:p+ct")
            .contains("template space 'p' (with"));
        assert_eq!(describe_stage_outcome("ok:custom"), "Done (custom).");
        assert_eq!(describe_stage_outcome("error: boom"), "error: boom");
    }

    #[test]
    fn long_ok_results_are_cut() {
        let long = format!("ok:{}", "x".repeat(120));
        let d = describe_stage_outcome(&long);
        assert!(d.ends_with("\u{2026})."), "{d}");
    }

    #[test]
    fn friendly_lines_plain() {
        let planned = vec!["bids".to_owned(), "coreg".to_owned()];
        let mut f = Friendly::new("1", Path::new("/data"), &planned, true);
        assert!(f.header().contains("(2 steps)"));
        assert_eq!(
            f.before_stage("coreg", "12:00:00"),
            "  [12:00:00]  Starting: Coregistration (post-op CT or MR \u{2192} anchor)\u{2026}\n    May take several minutes."
        );
        let s = f.after_stage("bids", "plan:bids", 0.2, "12:00:01");
        assert!(
            s.contains("[x]  BIDS layout \u{2014} plan only (0s)."),
            "{s}"
        );
        assert!(s.contains("Progress: [* .]"), "{s}");
        let e = f.after_stage("coreg", "error: no ants", 3.0, "12:00:02");
        assert!(
            e.contains("[!]")
                && e.contains("failed after 3s.")
                && e.contains("Reason: error: no ants")
        );
        assert!(f
            .footer(2, 0, false, 1.25, "12:00:03")
            .contains("all 2 step(s) in 1.2s total."));
        assert!(f
            .footer(2, 1, false, 1.25, "12:00:03")
            .contains("all 2 step(s) in 1.2s total; 1 step(s) failed."));
        assert!(f
            .footer(1, 1, false, 1.0, "12:00:03")
            .contains("Pipeline stopped after 1 of 2 step(s) in 1.0s total; 1 step(s) failed."));
        assert!(f
            .footer(1, 0, true, 1.0, "12:00:03")
            .contains("Pipeline interrupted after 1 of 2 step(s)"));
    }

    #[test]
    fn reporter_assigns_planned_names_by_index() {
        let planned = vec!["bids".to_owned(), "coreg".to_owned()];
        let mut r = RunReporter::new(&planned, None, false);
        r.handle(RunEvent::StageStarted {
            stage: "bids".into(),
        });
        r.handle(RunEvent::LogLine {
            line: "chatter".into(),
        });
        r.handle(RunEvent::StageCompleted {
            stage: "bids".into(),
            result: Some("plan:bids".into()),
        });
        r.handle(RunEvent::StageStarted {
            stage: "coregister".into(),
        });
        r.handle(RunEvent::StageFailed {
            stage: "coregister".into(),
            message: "bad".into(),
        });
        r.handle(RunEvent::StageCompleted {
            stage: "x".into(),
            result: None,
        });
        r.record_failure("the pipeline thread panicked");
        let got: Vec<(&str, &str)> = r
            .rows
            .iter()
            .map(|(a, b, _)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("bids", "plan:bids"),
                ("coreg", "error: bad"),
                ("stage2", "ok:stage2"),
                ("stage3", "error: the pipeline thread panicked")
            ]
        );
    }
}
