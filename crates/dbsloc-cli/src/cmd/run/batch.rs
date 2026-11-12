// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use dbsloc_pipeline::batch::{
    expand_subjects, parse_subject_list, run_batch, BatchEvent, BatchItem, BatchOutcome,
    BatchPhase, BatchPolicy, BatchSummary, ErrorPolicy, ReviewPolicy,
};
use dbsloc_pipeline::options::RunOptions;
use serde_json::{json, Map, Value};

use super::{log_mode, warn_ants_flags_unused, RunArgs};
use crate::fail::{err_line, out_json, CmdResult, Fail};
use crate::run::ants_args::AntsParsed;
use crate::run::progress::{Friendly, RunReporter};
use crate::run::summary::{self, Summary};

/// The subjects of this run, in order: the ids on the command line, then those of
/// `--subjects-file`, or every subject of the dataset with `--all-subjects`.
pub fn subject_ids(args: &RunArgs, opts: &RunOptions) -> Result<Vec<String>, Fail> {
    let mut explicit = args.subj_ids.clone();
    if let Some(file) = &args.subjects_file {
        let text = std::fs::read_to_string(file).map_err(|e| {
            Fail::new(
                2,
                format!(
                    "dbsloc: error: cannot read --subjects-file {}: {e}",
                    file.display()
                ),
            )
        })?;
        explicit.extend(parse_subject_list(&text));
    }
    let ids = expand_subjects(
        &opts.bids_dir,
        opts.output_dir.as_deref(),
        &explicit,
        args.all_subjects,
    );
    if ids.is_empty() {
        let why = if args.all_subjects {
            format!(
                "no sub-* folders found under {}",
                args.dataset_root.display()
            )
        } else {
            "give at least one subject id, --subjects-file or --all-subjects".to_owned()
        };
        return Err(Fail::new(2, format!("dbsloc: error: {why}")));
    }
    Ok(ids)
}

fn review_policy(args: &RunArgs, opts: &RunOptions) -> ReviewPolicy {
    if !opts.checkreg {
        ReviewPolicy::Off
    } else if opts.headless || args.defer_review {
        ReviewPolicy::Defer
    } else {
        ReviewPolicy::Interactive
    }
}

fn subject_summary(
    outcome: &BatchOutcome,
    rows: &[(String, String, f64)],
    planned: &[String],
    apply: bool,
    profile: bool,
) -> Value {
    let fallback = json!({ "subj_id": outcome.subject });
    let plan = outcome.plan.as_ref().unwrap_or(&fallback);
    let mut v = summary::build(&Summary {
        plan,
        apply,
        planned,
        rows,
        profile,
        interrupted: outcome.phase == BatchPhase::Cancelled,
    });
    if let Some(m) = v.as_object_mut() {
        m.insert("phase".into(), json!(outcome.phase.as_str()));
        if let Some(msg) = &outcome.message {
            m.insert("message".into(), json!(msg));
        }
        if let Some(stage) = outcome.awaiting_stage {
            m.insert("awaiting_stage".into(), json!(stage.cli_name()));
        }
        m.insert(
            "seconds".into(),
            json!((outcome.elapsed.as_secs_f64() * 1000.0).round() / 1000.0),
        );
    }
    v
}

pub fn table(summary: &BatchSummary) -> String {
    let rows: Vec<[String; 4]> = summary
        .outcomes
        .iter()
        .map(|o| {
            [
                format!("sub-{}", o.subject),
                o.phase.label().to_owned(),
                format!("{}/{}/{}", o.stages_ok, o.stages_failed, o.stages_skipped),
                format!("{:.1}", o.elapsed.as_secs_f64()),
            ]
        })
        .collect();
    let head = ["subject", "phase", "ok/failed/skipped", "seconds"];
    let mut width = head.map(str::len);
    for r in &rows {
        for (w, c) in width.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let line = |cells: [&str; 4]| {
        let mut s = String::from(" ");
        for (i, (c, w)) in cells.iter().zip(width).enumerate() {
            s.push(' ');
            if i == 3 {
                s.push_str(&format!("{c:>w$}"));
            } else {
                s.push_str(&format!("{c:<w$}"));
            }
            if i < 3 {
                s.push_str(" |");
            }
        }
        s.trim_end().to_owned()
    };
    let mut out = format!(
        "\nBatch summary: {} subject(s) in {:.1}s\n",
        summary.outcomes.len(),
        summary.elapsed.as_secs_f64()
    );
    out.push_str(&line(head));
    out.push('\n');
    for r in &rows {
        out.push_str(&line([&r[0], &r[1], &r[2], &r[3]]));
        out.push('\n');
    }
    out
}

pub fn exit_code(summary: &BatchSummary, interrupted: bool) -> i32 {
    if interrupted {
        130
    } else if !summary.outcomes.is_empty()
        && summary
            .outcomes
            .iter()
            .all(|o| o.phase == BatchPhase::PlanError)
    {
        2
    } else if summary.any_failure() {
        1
    } else {
        0
    }
}

struct Current {
    subject: String,
    last: bool,
    reporter: RunReporter,
}

pub fn run_many(
    args: &RunArgs,
    ants: &AntsParsed,
    opts: &RunOptions,
    planned: &[String],
    subjects: &[String],
    cancel: &AtomicBool,
) -> CmdResult {
    warn_ants_flags_unused(!opts.plan_only, planned, ants);
    let policy = BatchPolicy {
        on_error: if args.stop_on_first_failure {
            ErrorPolicy::Stop
        } else {
            ErrorPolicy::Continue
        },
        review: review_policy(args, opts),
        review_setup: None,
    };
    let (do_progress, plain) = log_mode(&args.log);
    let items: Vec<BatchItem> = subjects.iter().map(BatchItem::new).collect();
    let apply = !opts.plan_only;
    let mut entries: Vec<Value> = Vec::with_capacity(items.len());
    let mut current: Option<Current> = None;

    let (tx, rx) = mpsc::channel::<BatchEvent>();
    let (summary, panicked) = std::thread::scope(|s| {
        let handle = s.spawn(|| {
            let summary = run_batch(opts, &items, &policy, &tx, Some(cancel));
            drop(tx);
            summary
        });
        for ev in rx {
            match ev {
                BatchEvent::SubjectStarted {
                    index,
                    total,
                    subject,
                } => {
                    if do_progress {
                        err_line(&format!(
                            "\n=== Subject {}/{total}: sub-{subject} ===",
                            index + 1
                        ));
                    }
                    let friendly = do_progress
                        .then(|| Friendly::new(&subject, &opts.bids_dir, planned, plain));
                    let reporter = RunReporter::new(planned, friendly, do_progress);
                    reporter.header();
                    current = Some(Current {
                        subject,
                        last: index + 1 >= total,
                        reporter,
                    });
                }
                BatchEvent::Run(_, ev) => {
                    if let Some(c) = current.as_mut() {
                        c.reporter.handle(ev);
                    }
                }
                BatchEvent::SubjectFinished(outcome) => {
                    let mine = current.take().filter(|c| c.subject == outcome.subject);
                    let last = mine.as_ref().is_none_or(|c| c.last);
                    let mine = mine.map(|c| c.reporter);
                    let rows = match &mine {
                        Some(r) => {
                            if outcome.phase != BatchPhase::PlanError {
                                r.footer(
                                    summary::total_seconds(&r.rows),
                                    outcome.phase == BatchPhase::Cancelled,
                                );
                            }
                            r.rows.clone()
                        }
                        None => Vec::new(),
                    };
                    if outcome.phase.is_failure() {
                        let next = if policy.on_error == ErrorPolicy::Stop {
                            " The batch stops here (--stop-on-first-failure)."
                        } else if last {
                            ""
                        } else {
                            " Moving on to the next subject."
                        };
                        err_line(&format!(
                            "dbsloc: warning: sub-{} failed: {}.{next}",
                            outcome.subject,
                            outcome
                                .message
                                .as_deref()
                                .unwrap_or("see the messages above")
                                .trim_end_matches('.')
                        ));
                    }
                    entries.push(subject_summary(
                        &outcome,
                        &rows,
                        planned,
                        apply,
                        args.profile,
                    ));
                }
            }
        }
        match handle.join() {
            Ok(summary) => (summary, false),
            Err(_) => (BatchSummary::default(), true),
        }
    });
    if panicked {
        err_line("dbsloc: error: the batch thread panicked");
        return Ok(1);
    }
    let interrupted = cancel.load(Ordering::SeqCst);
    err_line(&table(&summary));
    let awaiting = summary.awaiting_qc();
    if !awaiting.is_empty() {
        err_line(&format!(
            "Awaiting registration review: {}. Approve each in `dbsloc viz checkreg review --manifest <locks folder>/checkreg_manifest.json` (or in the app), then run those subjects again.",
            awaiting
                .iter()
                .map(|s| format!("sub-{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut payload = Map::new();
    payload.insert("batch".into(), json!(true));
    payload.insert("subjects".into(), Value::Array(entries));
    payload.insert("totals".into(), summary.totals_json());
    payload.insert("awaiting_qc".into(), json!(awaiting));
    if interrupted {
        payload.insert("interrupted".into(), json!(true));
    }
    out_json(&Value::Object(payload));
    if interrupted {
        err_line("dbsloc: interrupted");
    }
    Ok(exit_code(&summary, interrupted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn outcome(subject: &str, phase: BatchPhase) -> BatchOutcome {
        BatchOutcome {
            subject: subject.to_owned(),
            phase,
            stages_ok: 2,
            stages_failed: u32::from(phase == BatchPhase::Failed),
            stages_skipped: 0,
            message: None,
            awaiting_stage: None,
            elapsed: Duration::from_millis(1500),
            plan: None,
        }
    }

    fn summary_of(phases: &[BatchPhase]) -> BatchSummary {
        BatchSummary {
            outcomes: phases
                .iter()
                .enumerate()
                .map(|(i, p)| outcome(&format!("{i:02}"), *p))
                .collect(),
            elapsed: Duration::from_secs(3),
        }
    }

    #[test]
    fn exit_codes_follow_the_subject_phases() {
        use BatchPhase::*;
        assert_eq!(exit_code(&summary_of(&[Done, Done]), false), 0);
        assert_eq!(exit_code(&summary_of(&[Done, AwaitingQc]), false), 0);
        assert_eq!(exit_code(&summary_of(&[Done, Failed]), false), 1);
        assert_eq!(exit_code(&summary_of(&[Done, PlanError]), false), 1);
        assert_eq!(exit_code(&summary_of(&[PlanError, PlanError]), false), 2);
        assert_eq!(exit_code(&summary_of(&[Done, Cancelled]), true), 130);
    }

    #[test]
    fn the_table_has_one_aligned_row_per_subject() {
        let t = table(&summary_of(&[BatchPhase::Done, BatchPhase::AwaitingQc]));
        let lines: Vec<&str> = t.lines().filter(|l| l.contains('|')).collect();
        assert_eq!(lines.len(), 3, "{t}");
        assert!(lines[0].contains("subject") && lines[0].contains("ok/failed/skipped"));
        assert!(lines[2].contains("sub-01") && lines[2].contains("awaiting review"));
        assert!(lines[1].contains("2/0/0") && lines[1].ends_with("1.5"));
        let bar = |l: &str| l.find('|');
        assert_eq!(bar(lines[0]), bar(lines[1]));
    }
}
