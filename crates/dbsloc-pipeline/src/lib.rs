// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pipeline orchestration: ``RunOptions``, stage dispatch, and lock files.
//!
//! The one pipeline implementation: the GUI, `dbsloc_pipeline_runner` and the Python
//! `dbsloc run` (through the `dbsloc_reconstruct` extension) all drive [`run_pipeline_channel`].
//! See `specs/00_overview.md`.
//!
//! # Key public API
//! - [`run_pipeline_channel`] — run stages in-process, streaming events to a channel.
//! - [`plan_run`] — subject, template, locks directory and lock inputs of a run, no stage work.
//! - [`bids::resolve_subject_with`] / [`bids::list_subject_ids`] — BIDS path resolution.
//! - [`stages::Stage`] — pipeline stage enum.
//! - [`electrode_model_names`] — catalogue names for `RunOptions::el_model`.

pub mod ants_argv;
pub mod atlas_catalog;
pub mod atlas_fetch;
pub mod atlas_index;
pub mod backdrop_catalog;
pub mod backdrop_fetch;
pub mod backdrops;
pub mod batch;
pub mod bids;
pub mod bundled_bin;
pub mod checkreg;
pub mod connectome_archive;
pub mod connectome_cache;
pub mod connectome_catalog;
pub mod connectome_fetch;
pub mod connectome_maps;
pub mod coreg_multistart;
pub mod ct_mask;
pub mod ct_recon;
pub mod derivatives;
pub mod display;
pub mod error;
pub mod group;
pub mod import;
pub mod invalidate;
pub mod leaddbs_data;
pub mod legacy_env;
pub mod locks;
pub mod manual_correction;
pub mod md5;
pub mod nifti_io;
pub mod norm_grid;
pub(crate) mod norm_native;
pub mod options;
pub mod orientation_stage;
pub mod outputs;
pub mod paths;
pub mod point_warp;
pub mod precoreg;
pub mod prereqs;
pub mod protocol;
#[cfg(feature = "pyo3")]
pub mod python;
pub mod reco_mat;
pub mod reg_backend;
pub mod reports;
pub mod resample;
pub mod sha1;
pub mod sha256;
pub mod space_bridge;
pub mod stage_impl;
pub mod stages;
pub mod subject_summary;
pub mod template_catalog;
pub mod template_contacts;
pub mod template_fetch;
pub mod templates;
pub mod tutor;
pub mod uiprefs;
pub mod vat;
pub mod version;

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use serde_json::{json, Value};

use crate::bids::{BidsError, Subject};
pub use crate::checkreg::{
    apply_approval, apply_decisions, finish_review, method_log_path, prepare_review, review_state,
    slide_approved, slide_rejected, PngWriter, ReviewHook, ReviewOutcome, ReviewRequest,
    ReviewSetup, ReviewState,
};
use crate::locks::{
    default_lock_input_paths, lock_allows_skip_with, stage_approval_path, HashMemo,
};
pub use crate::manual_correction::{
    apply_manual_correction, file_edit_space, inspect_manual_correction, prepare_manual_correction,
    ContextSide, CorrectionReport, CorrectionRequest, EditSpace, ManualCorrectionContext, SideEdit,
    SideReport,
};
pub use crate::norm_grid::{NormGridSource, ResolvedNormGrid};
use crate::options::RunOptions;
use crate::protocol::{RunEvent, StageOutcome};
pub use crate::resample::{norm_grid_dims, norm_grid_warning, template_voxel_mm};
use crate::stages::{plan_outcome, run_stage, run_stage_with_review, Stage};

/// Electrode catalogue display names, for populating an `el_model` picker.
pub fn electrode_model_names() -> Vec<&'static str> {
    dbsloc_reconstruct::electrode_model_names()
}

#[derive(Debug, Clone)]
pub struct RunPlan {
    pub subject: Subject,
    pub template: Option<PathBuf>,
    pub template_t2: Option<PathBuf>,
    pub lock_input_paths: Option<BTreeMap<String, Vec<PathBuf>>>,
    pub warnings: Vec<String>,
    /// The registration grid `normalize` would use (`specs/04` §5.1 step 2).
    pub norm_grid: norm_grid::ResolvedNormGrid,
}

impl RunPlan {
    pub fn to_json(&self, opts: &RunOptions) -> Value {
        let lip = self.lock_input_paths.as_ref().map(|m| {
            Value::Object(
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            json!(v
                                .iter()
                                .map(|p| p.to_string_lossy().into_owned())
                                .collect::<Vec<_>>()),
                        )
                    })
                    .collect(),
            )
        });
        json!({
            "subj_id": self.subject.subj_id,
            "subj_dir": self.subject.subj_dir.to_string_lossy(),
            "output_dir": opts.derivatives_root().to_string_lossy(),
            "locks_dir": self.subject.locks_dir().to_string_lossy(),
            "stages": opts.stages.iter().map(|s| s.cli_name()).collect::<Vec<_>>(),
            "template": self.template.as_ref().map(|t| t.to_string_lossy().into_owned()),
            "template_t2": self.template_t2.as_ref().map(|t| t.to_string_lossy().into_owned()),
            "norm_multispectral": opts.norm_multispectral,
            "norm_isotropic_mm": self.norm_grid.mm,
            "norm_grid_source": self.norm_grid.source.as_str(),
            "lock_input_paths": lip,
            "warnings": self.warnings,
            "subject": self.subject.to_json(),
        })
    }
}

pub fn plan_run(opts: &RunOptions) -> Result<RunPlan, BidsError> {
    let subject = opts.resolve_subject()?;
    let lock_input_paths = opts
        .fingerprint_locks
        .then(|| default_lock_input_paths(&subject));
    let resolved = opts.with_resolved_template();
    let mut warnings: Vec<String> = resample::norm_grid_warning(&resolved).into_iter().collect();
    let plans_normalize = resolved.stages.contains(&Stage::Normalize);
    let template_readable = resolved
        .normalization_template
        .as_deref()
        .and_then(norm_grid::template_default_mm)
        .is_some();
    let caps = (plans_normalize && resolved.norm_isotropic_mm.is_template() && template_readable)
        .then(|| reg_backend::planning_capacity_caps(resolved.backend_for(Stage::Normalize)))
        .flatten();
    let channels = stage_impl::norm_channel_count(&subject, &resolved);
    let norm_grid = norm_grid::resolve(&resolved, channels, caps.as_ref());
    if let (true, Some(note)) = (plans_normalize, &norm_grid.note) {
        warnings.push(format!("normalize: {note}; {}", norm_grid::STEP_REMEDY));
    }
    Ok(RunPlan {
        warnings,
        template: resolved.normalization_template,
        template_t2: resolved.normalization_template_t2,
        subject,
        lock_input_paths,
        norm_grid,
    })
}

fn approved_outcome() -> StageOutcome {
    StageOutcome::skipped_as("approved in locks/ with unchanged inputs", "skip:approved")
}

fn backfill_norm_affines<F: FnMut(&RunEvent)>(sub: &bids::Subject, sink: &mut F) {
    let line = match stage_impl::backfill_norm_affines(sub) {
        Ok(written) if written.is_empty() => return,
        Ok(written) => format!(
            "dbsloc: note: normalize: wrote the linear part of the approved normalization: {}",
            written
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(e) => format!(
            "dbsloc: warning: normalize: the linear part of the approved normalization was not written: {e}"
        ),
    };
    sink(&RunEvent::LogLine { line });
}

/// Emit the events for a failed stage and return its contribution to the failure tally (always 1).
///
/// A [`error::PipelineError::QcPending`] first emits [`RunEvent::StageAwaitingQc`] so a server can
/// mark the run `awaiting_qc` and resume after approval; the [`RunEvent::StageFailed`] that follows
/// carries the same message text the CLI and GUI have always printed, so their output is unchanged.
fn emit_stage_failure<F: FnMut(&RunEvent)>(
    sink: &mut F,
    stage: Stage,
    e: &error::PipelineError,
) -> u32 {
    if matches!(e, error::PipelineError::QcPending { .. }) {
        sink(&RunEvent::StageAwaitingQc {
            stage: stage.label().to_owned(),
        });
    }
    sink(&RunEvent::StageFailed {
        stage: stage.label().to_owned(),
        message: e.to_string(),
    });
    1
}

fn approval_is_external(stage: Stage) -> bool {
    matches!(
        stage,
        Stage::Preprocess
            | Stage::Precoreg
            | Stage::Coregister
            | Stage::Normalize
            | Stage::Brainshift
    )
}

fn review_log(sub: &Subject, stage: Stage) -> Option<PathBuf> {
    match stage {
        Stage::Coregister => Some(checkreg::default_coreg_method_log_path(sub)),
        Stage::Normalize => Some(checkreg::default_norm_method_log_path(sub)),
        Stage::Brainshift => Some(checkreg::default_brainshift_method_log_path(sub)),
        _ => None,
    }
}

fn stage_markers(sub: &Subject, stage: Stage) -> Vec<PathBuf> {
    match stage {
        Stage::Preprocess => vec![
            derivatives::preproc_image(sub),
            derivatives::preproc_record(sub),
        ],
        Stage::Precoreg => vec![
            derivatives::precoreg_image(sub),
            derivatives::precoreg_record(sub),
        ],
        _ => review_log(sub, stage).into_iter().collect(),
    }
}

type FileState = Option<(u64, std::time::SystemTime)>;

fn file_state(path: &std::path::Path) -> FileState {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    Some((meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)))
}

fn watch_roots(sub: &Subject, stage: Stage) -> Vec<PathBuf> {
    let mut roots = vec![sub.subj_dir.clone()];
    if stage == Stage::Brainshift {
        roots.extend(
            std::env::var_os("DBSLOC_SCRF_OUTPUT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        );
    }
    roots
}

struct Attempt {
    stage: Stage,
    lock: PathBuf,
    approved_bytes: Option<Vec<u8>>,
    markers: Vec<(PathBuf, FileState)>,
    watch: outputs::ReplaceWatch,
}

impl Attempt {
    fn begin<F: FnMut(&RunEvent)>(
        sub: &Subject,
        stage: Stage,
        sink: &mut F,
    ) -> Result<Attempt, String> {
        let name = stage.cli_name();
        let markers = stage_markers(sub, stage)
            .into_iter()
            .map(|p| {
                let state = file_state(&p);
                (p, state)
            })
            .collect();
        let lock = stage_approval_path(&sub.locks_dir(), name);
        let bytes = match std::fs::read(&lock) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("cannot read {}: {e}", lock.display())),
        };
        let approved = bytes.filter(|b| {
            serde_json::from_slice::<Value>(b)
                .ok()
                .is_some_and(|v| locks::approval_flag(&v))
        });
        let mut approved_bytes = None;
        if let Some(b) = approved {
            if invalidate::invalidate_stage_lock(sub, stage, name)? {
                approved_bytes = Some(b);
                sink(&RunEvent::LogLine {
                    line: format!(
                        "dbsloc: {name} runs again; its approval is withdrawn until the new result is approved"
                    ),
                });
            }
        }
        Ok(Attempt {
            stage,
            lock,
            approved_bytes,
            markers,
            watch: outputs::ReplaceWatch::new(&watch_roots(sub, stage)),
        })
    }

    fn markers_changed(&self) -> bool {
        self.markers.iter().any(|(p, s)| file_state(p) != *s)
    }

    fn committed(&self) -> bool {
        self.watch.replaced() || self.markers_changed()
    }

    fn reviewed(&self, sub: &Subject, opts: &RunOptions) -> bool {
        opts.checkreg
            && self.markers_changed()
            && review_log(sub, self.stage)
                .is_some_and(|log| checkreg::method_log_allows_continue(&log))
    }

    fn still_withdrawn_by_us(&self) -> bool {
        locks::read_approval(&self.lock)
            .ok()
            .flatten()
            .is_some_and(|v| {
                !locks::approval_flag(&v)
                    && v.get("invalidated_by").and_then(Value::as_str)
                        == Some(self.stage.cli_name())
            })
    }

    fn restored_bytes(&self, bytes: &[u8], inputs: Option<&[PathBuf]>) -> Result<Vec<u8>, String> {
        let Some(paths) = inputs.filter(|p| !p.is_empty()) else {
            return Ok(bytes.to_vec());
        };
        let mut value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| "the lock is not a JSON object".to_owned())?;
        obj.insert(
            "inputs".into(),
            Value::Array(locks::snap_input_paths(paths)?),
        );
        let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        Ok((text + "\n").into_bytes())
    }

    fn restore<F: FnMut(&RunEvent)>(&self, why: &str, inputs: Option<&[PathBuf]>, sink: &mut F) {
        let Some(bytes) = &self.approved_bytes else {
            return;
        };
        let name = self.stage.cli_name();
        let line = if !self.still_withdrawn_by_us() {
            format!(
                "dbsloc: note: {name}: {why}, but {} changed while the step ran; it is left as it is",
                self.lock.display()
            )
        } else {
            match self.restored_bytes(bytes, inputs).and_then(|b| {
                outputs::write_replacing(&self.lock, &b)
                    .map_err(|e| format!("cannot rewrite {}: {e}", self.lock.display()))
            }) {
                Ok(()) if inputs.is_some_and(|p| !p.is_empty()) => format!(
                    "dbsloc: note: {name}: {why}; its approval is kept, with the input fingerprints of this run"
                ),
                Ok(()) => format!("dbsloc: note: {name}: {why}; its approval is kept"),
                Err(e) => format!("dbsloc: warning: {name}: {why}, but the approval was not restored: {e}"),
            }
        };
        sink(&RunEvent::LogLine { line });
    }
}

fn fresh_lock_inputs(opts: &RunOptions, stage: Stage) -> Option<Vec<PathBuf>> {
    if !opts.fingerprint_locks {
        return None;
    }
    let sub = opts.resolve_subject().ok()?;
    default_lock_input_paths(&sub).remove(stage.cli_name())
}

fn invalidate_after<F: FnMut(&RunEvent)>(
    sink: &mut F,
    plan: &Result<RunPlan, BidsError>,
    opts: &RunOptions,
    stage: Stage,
) {
    if let (Ok(p), false) = (plan, opts.plan_only) {
        let inv = invalidate::invalidate_dependents(&p.subject, stage);
        for line in inv.warnings.iter().cloned().chain(inv.log_line(stage)) {
            sink(&RunEvent::LogLine { line });
        }
    }
}

type StageRunner = dyn Fn(
    &RunOptions,
    Stage,
    Option<&mpsc::Sender<RunEvent>>,
    Option<&AtomicBool>,
) -> error::PipelineResult<StageOutcome>;

fn run_pipeline_with_emit<F: FnMut(&RunEvent)>(
    opts: &RunOptions,
    sink: F,
    log_tx: Option<&mpsc::Sender<RunEvent>>,
    cancel: Option<&AtomicBool>,
) -> u32 {
    run_pipeline_with_runner(opts, sink, log_tx, cancel, &run_stage)
}

fn run_pipeline_with_runner<F: FnMut(&RunEvent)>(
    opts: &RunOptions,
    mut sink: F,
    log_tx: Option<&mpsc::Sender<RunEvent>>,
    cancel: Option<&AtomicBool>,
    runner: &StageRunner,
) -> u32 {
    let opts = &opts.with_resolved_template();
    let plan = plan_run(opts);
    let mut ok: u32 = 0;
    let mut failed: u32 = 0;
    let mut skipped: u32 = 0;
    let mut memo = HashMemo::default();
    if let Ok(p) = &plan {
        for w in &p.warnings {
            sink(&RunEvent::LogLine {
                line: format!("dbsloc: warning: {w}"),
            });
        }
    }
    for &stage in &opts.stages {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            sink(&RunEvent::LogLine {
                line: "⏹ cancelled — remaining stages skipped".into(),
            });
            break;
        }
        sink(&RunEvent::StageStarted {
            stage: stage.label().to_owned(),
        });
        let mut attempt: Option<Attempt> = None;
        let outcome = match &plan {
            Ok(p) => {
                let deps = p
                    .lock_input_paths
                    .as_ref()
                    .and_then(|m| m.get(stage.cli_name()))
                    .map(Vec::as_slice);
                let lock = stage_approval_path(&p.subject.locks_dir(), stage.cli_name());
                let mut skip = if opts.overwrite_approved {
                    false
                } else {
                    match lock_allows_skip_with(&lock, deps, &mut memo) {
                        Ok(s) => s,
                        Err(e) => {
                            sink(&RunEvent::LogLine {
                                line: format!("dbsloc: warning: {e}; the stage runs"),
                            });
                            false
                        }
                    }
                };
                if skip {
                    let mismatch = match stage {
                        Stage::Preprocess => {
                            stage_impl::preprocess_approved_outputs_match(&p.subject, opts).err()
                        }
                        Stage::Normalize => {
                            stage_impl::approved_outputs_match(&p.subject, opts).err()
                        }
                        Stage::Coregister => {
                            stage_impl::coreg_approved_outputs_match(&p.subject, opts).err()
                        }
                        Stage::Precoreg => {
                            precoreg::precoreg_approved_outputs_match(&p.subject, opts).err()
                        }
                        _ => None,
                    };
                    if let Some(why) = mismatch {
                        sink(&RunEvent::LogLine {
                            line: format!(
                                "dbsloc: note: {}: {why}; the stage runs",
                                stage.cli_name()
                            ),
                        });
                        skip = false;
                    }
                }
                if skip {
                    if stage == Stage::Normalize && !opts.plan_only {
                        backfill_norm_affines(&p.subject, &mut sink);
                    }
                    Ok(approved_outcome())
                } else {
                    let begun = if !opts.plan_only && approval_is_external(stage) {
                        Attempt::begin(&p.subject, stage, &mut sink).map(Some)
                    } else {
                        Ok(None)
                    };
                    match begun {
                        Ok(a) => {
                            attempt = a;
                            runner(opts, stage, log_tx, cancel)
                        }
                        Err(e) => Err(error::PipelineError::Message(format!(
                            "{}: cannot withdraw its approval before it runs again: {e}",
                            stage.cli_name()
                        ))),
                    }
                }
            }
            Err(_) if opts.plan_only => Ok(plan_outcome(stage)),
            Err(e) => Err(error::PipelineError::Bids(e.message.clone())),
        };
        if matches!(
            stage,
            Stage::Preprocess | Stage::Precoreg | Stage::Coregister | Stage::Normalize
        ) {
            reg_backend::release_idle_gpu_memory();
        }
        match outcome {
            Ok(StageOutcome::Completed(result)) => {
                ok += 1;
                sink(&RunEvent::StageCompleted {
                    stage: stage.label().to_owned(),
                    result: Some(result),
                });
                if let (Some(a), Ok(p)) = (&attempt, &plan) {
                    if a.reviewed(&p.subject, opts) {
                        let inputs = fresh_lock_inputs(opts, stage);
                        a.restore(
                            "the new result was approved in its review during this run",
                            inputs.as_deref(),
                            &mut sink,
                        );
                    }
                }
                invalidate_after(&mut sink, &plan, opts, stage);
            }
            Ok(StageOutcome::Skipped { reason, result }) => {
                skipped += 1;
                match &attempt {
                    Some(a) if a.committed() => invalidate_after(&mut sink, &plan, opts, stage),
                    Some(a) => a.restore("the step did not run", None, &mut sink),
                    None => {}
                }
                sink(&RunEvent::StageSkipped {
                    stage: stage.label().to_owned(),
                    reason,
                    result: Some(result),
                });
            }
            Err(e) => {
                let pending = matches!(e, error::PipelineError::QcPending { .. });
                let committed = attempt.as_ref().is_some_and(Attempt::committed);
                if pending || committed {
                    invalidate_after(&mut sink, &plan, opts, stage);
                } else if let Some(a) = &attempt {
                    a.restore(
                        "the step stopped before it replaced any output",
                        None,
                        &mut sink,
                    );
                }
                failed += emit_stage_failure(&mut sink, stage, &e);
                if opts.stop_on_error || pending || cancel.is_some_and(|c| c.load(Ordering::SeqCst))
                {
                    break;
                }
            }
        }
    }
    sink(&RunEvent::PipelineCompleted {
        stages_ok: ok,
        stages_failed: failed,
        stages_skipped: skipped,
    });
    failed
}

/// Push each event through `tx` (e.g. GUI log). Same in-process driver as `dbsloc_pipeline_runner`.
/// Optional `cancel`: when set and becomes `true`, the running external tool is killed and
/// the remaining stages are skipped.
pub fn run_pipeline_channel(
    opts: &RunOptions,
    tx: &mpsc::Sender<RunEvent>,
    cancel: Option<&AtomicBool>,
) -> u32 {
    run_pipeline_with_emit(
        opts,
        |ev| {
            let _ = tx.send(ev.clone());
        },
        Some(tx),
        cancel,
    )
}

pub fn run_pipeline_channel_with_review(
    opts: &RunOptions,
    tx: &mpsc::Sender<RunEvent>,
    cancel: Option<&AtomicBool>,
    review: ReviewSetup,
) -> u32 {
    let runner = move |o: &RunOptions,
                       stage: Stage,
                       log: Option<&mpsc::Sender<RunEvent>>,
                       c: Option<&AtomicBool>| {
        run_stage_with_review(o, stage, log, c, Some(&review))
    };
    run_pipeline_with_runner(
        opts,
        |ev| {
            let _ = tx.send(ev.clone());
        },
        Some(tx),
        cancel,
        &runner,
    )
}

const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
const SUBPROCESS_POLL: std::time::Duration = std::time::Duration::from_millis(25);

fn lock_child(
    child: &std::sync::Mutex<std::process::Child>,
) -> std::sync::MutexGuard<'_, std::process::Child> {
    child.lock().unwrap_or_else(|p| p.into_inner())
}

/// Spawn `dbsloc_pipeline_runner` from a prepared `Command` (program and environment) and stream
/// its ``RunEvent`` lines into `tx`; stdin and stdout are set to pipes here.
///
/// The options are written as one JSON line and stdin stays open. When `cancel` becomes `true` the
/// runner is sent a `cancel` line so it stops its own tools, and is killed if it has not exited
/// after a grace period. Every exit path kills a still-running child, reaps it and joins the
/// watcher thread.
pub fn run_pipeline_subprocess_cmd(
    opts: &RunOptions,
    tx: mpsc::Sender<RunEvent>,
    cancel: std::sync::Arc<AtomicBool>,
    mut cmd: std::process::Command,
) -> std::io::Result<std::process::ExitStatus> {
    use std::sync::{Arc, Mutex};

    let mut opts_json = serde_json::to_vec(opts)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    opts_json.push(b'\n');

    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();

    if let Some(w) = stdin.as_mut() {
        if let Err(e) = w.write_all(&opts_json).and_then(|()| w.flush()) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    }

    let child = Arc::new(Mutex::new(child));
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let child = Arc::clone(&child);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while !stop.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    if let Some(w) = stdin.as_mut() {
                        let _ = w.write_all(b"cancel\n").and_then(|()| w.flush());
                    }
                    let deadline = std::time::Instant::now() + CANCEL_GRACE;
                    while !stop.load(Ordering::SeqCst) {
                        if std::time::Instant::now() >= deadline {
                            let _ = lock_child(&child).kill();
                            break;
                        }
                        std::thread::sleep(SUBPROCESS_POLL);
                    }
                    return;
                }
                std::thread::sleep(SUBPROCESS_POLL);
            }
        })
    };

    let mut read_err: Option<std::io::Error> = None;
    if let Some(stdout) = stdout {
        let mut reader = std::io::BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if let Ok(ev) = serde_json::from_str::<RunEvent>(line.trim_end()) {
                        let _ = tx.send(ev);
                    }
                }
                Err(e) => {
                    read_err = Some(e);
                    break;
                }
            }
        }
    }
    if read_err.is_some() {
        let _ = lock_child(&child).kill();
    }

    let status = loop {
        let polled = lock_child(&child).try_wait();
        match polled {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(SUBPROCESS_POLL),
            Err(e) => {
                let mut c = lock_child(&child);
                let _ = c.kill();
                let _ = c.wait();
                break Err(e);
            }
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    match read_err {
        Some(e) => Err(e),
        None => status,
    }
}

pub fn stage_names() -> Vec<&'static str> {
    Stage::default_sequence()
        .iter()
        .map(|s| s.cli_name())
        .collect()
}

#[cfg(test)]
mod driver_tests {
    use super::*;
    use crate::bids::resolve_subject_with;
    use std::path::{Path, PathBuf};

    fn run_pipeline_collect(opts: &RunOptions) -> (u32, Vec<RunEvent>) {
        let (tx, rx) = mpsc::channel::<RunEvent>();
        let failed = run_pipeline_with_emit(
            opts,
            |ev| {
                let _ = tx.send(ev.clone());
            },
            Some(&tx),
            None,
        );
        drop(tx);
        (failed, rx.into_iter().collect())
    }

    fn plan_only(stages: Vec<Stage>) -> RunOptions {
        RunOptions {
            stages,
            headless: true,
            no_scene3d: true,
            plan_only: true,
            stop_on_error: false,
            ..RunOptions::new("/nonexistent/bids", "01")
        }
    }

    fn outcomes(events: &[RunEvent]) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|ev| match ev {
                RunEvent::StageCompleted { stage, result } => {
                    Some((stage.clone(), result.clone().unwrap_or_default()))
                }
                RunEvent::StageSkipped { stage, result, .. } => {
                    Some((stage.clone(), result.clone().unwrap_or_default()))
                }
                RunEvent::StageFailed { stage, message } => {
                    Some((stage.clone(), format!("error: {message}")))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn collect_reports_every_stage_in_order() {
        let stages = vec![Stage::Preprocess, Stage::Coregister, Stage::Normalize];
        let (failed, events) = run_pipeline_collect(&plan_only(stages.clone()));
        assert_eq!(failed, 0);

        let mut seen: Vec<(&str, String)> = Vec::new();
        for ev in &events {
            match ev {
                RunEvent::StageStarted { stage } => seen.push(("started", stage.clone())),
                RunEvent::StageSkipped { stage, .. } => seen.push(("skipped", stage.clone())),
                RunEvent::StageCompleted { stage, .. } => seen.push(("completed", stage.clone())),
                RunEvent::StageFailed { stage, .. } => seen.push(("failed", stage.clone())),
                _ => {}
            }
        }
        assert_eq!(seen.len(), 2 * stages.len(), "{seen:?}");
        for (i, stage) in stages.iter().enumerate() {
            assert_eq!(seen[2 * i], ("started", stage.label().to_owned()));
            assert_eq!(
                seen[2 * i].1,
                seen[2 * i + 1].1,
                "outcome follows its start"
            );
        }
        assert!(matches!(
            events.last(),
            Some(RunEvent::PipelineCompleted { .. })
        ));
    }

    #[test]
    fn stage_logs_reach_the_collector() {
        let (_failed, events) = run_pipeline_collect(&plan_only(vec![Stage::Reconstruct]));
        assert!(
            events.iter().any(|e| matches!(
                e,
                RunEvent::StageFailed { .. } | RunEvent::StageSkipped { .. }
            )),
            "{events:?}"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_driver_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
        d
    }

    fn run(root: &Path, stages: Vec<Stage>, plan: bool, overwrite: bool) -> Vec<(String, String)> {
        let opts = RunOptions {
            stages,
            plan_only: plan,
            overwrite_approved: overwrite,
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            ..RunOptions::new(root, "1")
        };
        outcomes(&run_pipeline_collect(&opts).1)
    }

    fn completing(
        _: &RunOptions,
        _: Stage,
        _: Option<&mpsc::Sender<RunEvent>>,
        _: Option<&AtomicBool>,
    ) -> error::PipelineResult<StageOutcome> {
        Ok(StageOutcome::Completed("ok:fake".into()))
    }

    #[test]
    fn finer_than_template_grid_logs_a_warning_once() {
        let root = scratch("finegrid");
        let tpl = root.join("tpl.nii.gz");
        let hdr = nifti::NiftiHeader {
            dim: [3, 8, 6, 4, 1, 1, 1, 1],
            pixdim: [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            sform_code: 1,
            srow_x: [1.0, 0.0, 0.0, 0.0],
            srow_y: [0.0, 1.0, 0.0, 0.0],
            srow_z: [0.0, 0.0, 1.0, 0.0],
            scl_slope: 1.0,
            ..nifti::NiftiHeader::default()
        };
        nifti::writer::WriterOptions::new(&tpl)
            .reference_header(&hdr)
            .write_nifti(&ndarray::Array3::<f32>::zeros((8, 6, 4)))
            .unwrap();
        let mk = |mm: f64| RunOptions {
            stages: vec![Stage::Normalize],
            plan_only: true,
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            normalization_template: Some(tpl.clone()),
            norm_isotropic_mm: options::NormGrid::Mm(mm),
            ..RunOptions::new(&root, "1")
        };
        let count = |opts: &RunOptions| {
            let mut events = Vec::new();
            run_pipeline_with_runner(opts, |e| events.push(e.clone()), None, None, &completing);
            events
                .iter()
                .filter(|e| {
                    matches!(e, RunEvent::LogLine { line }
                        if line.starts_with("dbsloc: warning: normalize: registration grid 0.5 mm is finer"))
                })
                .count()
        };
        assert_eq!(count(&mk(0.5)), 1);
        assert_eq!(count(&mk(1.0)), 0);
        let plan = plan_run(&mk(0.5)).unwrap();
        assert_eq!(plan.warnings.len(), 1);
        assert_eq!(
            plan.to_json(&mk(0.5))["warnings"].as_array().unwrap().len(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn qc_pending_ends_the_run_even_without_stop_on_error() {
        let root = scratch("qc_pending");
        let opts = RunOptions {
            stages: vec![Stage::Coregister, Stage::Normalize],
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            ..RunOptions::new(&root, "1")
        };
        let pending = |_: &RunOptions,
                       _: Stage,
                       _: Option<&mpsc::Sender<RunEvent>>,
                       _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            Err(error::PipelineError::QcPending {
                stage: "coreg".to_owned(),
                manifest: "approve in the viewer".to_owned(),
            })
        };
        let mut events = Vec::new();
        let failed =
            run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, &pending);
        assert_eq!(failed, 1);
        let kinds: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::StageStarted { stage } => Some(format!("started:{stage}")),
                RunEvent::StageAwaitingQc { stage } => Some(format!("awaiting:{stage}")),
                RunEvent::StageFailed { stage, .. } => Some(format!("failed:{stage}")),
                RunEvent::PipelineCompleted {
                    stages_ok,
                    stages_failed,
                    stages_skipped,
                } => Some(format!(
                    "completed:{stages_ok}/{stages_failed}/{stages_skipped}"
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "started:coregister",
                "awaiting:coregister",
                "failed:coregister",
                "completed:0/1/0"
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn qc_pending_invalidates_dependents_and_a_plain_failure_does_not() {
        let root = scratch("inval_qc");
        let locks = lock_dir(&root);
        let approve = || {
            for stage in ["normalize", "reconstruct"] {
                std::fs::write(locks.join(format!("{stage}.json")), r#"{"approval": true}"#)
                    .unwrap();
            }
        };
        let opts = RunOptions {
            stages: vec![Stage::Coregister],
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            ..RunOptions::new(&root, "1")
        };
        let failing = |_: &RunOptions,
                       _: Stage,
                       _: Option<&mpsc::Sender<RunEvent>>,
                       _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            Err(error::PipelineError::Message(
                "antsRegistration failed with 1".to_owned(),
            ))
        };
        approve();
        let mut events = Vec::new();
        run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, &failing);
        assert!(invalidation_lines(&events).is_empty());
        assert_eq!(lock_json(&locks, "normalize")["approval"], json!(true));

        let pending = |_: &RunOptions,
                       _: Stage,
                       _: Option<&mpsc::Sender<RunEvent>>,
                       _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            Err(error::PipelineError::QcPending {
                stage: "coreg".to_owned(),
                manifest: "approve in the viewer".to_owned(),
            })
        };
        let mut events = Vec::new();
        run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, &pending);
        for stage in ["normalize", "reconstruct"] {
            let lock = lock_json(&locks, stage);
            assert_eq!(lock["approval"], json!(false), "{stage}");
            assert_eq!(lock["invalidated_by"], json!("coreg"), "{stage}");
        }
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: coreg re-ran; approvals invalidated: normalize, reconstruct"]
        );
        let awaiting = events
            .iter()
            .position(|e| matches!(e, RunEvent::StageAwaitingQc { .. }))
            .unwrap();
        let line = events
            .iter()
            .position(|e| matches!(e, RunEvent::LogLine { line } if line.contains("approvals invalidated")))
            .unwrap();
        assert!(line < awaiting);

        approve();
        let plan = RunOptions {
            plan_only: true,
            ..opts.clone()
        };
        run_pipeline_with_runner(&plan, |_| {}, None, None, &pending);
        assert_eq!(lock_json(&locks, "normalize")["approval"], json!(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn review_run_kinds(approve: bool) -> (u32, Vec<String>) {
        let root = scratch(if approve { "review_ok" } else { "review_no" });
        let opts = RunOptions {
            stages: vec![Stage::Coregister, Stage::Normalize],
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            ..RunOptions::new(&root, "1")
        };
        let hook: std::sync::Arc<ReviewHook> = std::sync::Arc::new(move |req, _| {
            match apply_decisions(
                &req.method_log,
                &req.branch,
                &[(Some("CT".to_owned()), approve)],
            ) {
                Ok(()) => ReviewOutcome::Decided,
                Err(e) => ReviewOutcome::Failed(e),
            }
        });
        let setup = ReviewSetup {
            hook,
            png: PngWriter::Viz,
        };
        let runner = move |o: &RunOptions,
                           stage: Stage,
                           _: Option<&mpsc::Sender<RunEvent>>,
                           c: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            if stage == Stage::Coregister {
                let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
                let mlog = checkreg::default_coreg_method_log_path(&sub);
                let env = checkreg::GateEnv::from_setup(Some(&setup));
                checkreg::viewer_and_gate_slides(&sub, "coreg", &mlog, false, &env, &[], c, None)?;
            }
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let (tx, rx) = mpsc::channel();
        let failed = run_pipeline_with_runner(
            &opts,
            |e| {
                let _ = tx.send(e.clone());
            },
            None,
            None,
            &runner,
        );
        drop(tx);
        let kinds = rx
            .iter()
            .filter_map(|e| match e {
                RunEvent::StageStarted { stage } => Some(format!("started:{stage}")),
                RunEvent::StageAwaitingQc { stage } => Some(format!("awaiting:{stage}")),
                RunEvent::StageFailed { stage, .. } => Some(format!("failed:{stage}")),
                RunEvent::StageCompleted { stage, .. } => Some(format!("done:{stage}")),
                RunEvent::PipelineCompleted {
                    stages_ok,
                    stages_failed,
                    stages_skipped,
                } => Some(format!(
                    "completed:{stages_ok}/{stages_failed}/{stages_skipped}"
                )),
                _ => None,
            })
            .collect();
        let _ = std::fs::remove_dir_all(&root);
        (failed, kinds)
    }

    #[test]
    fn approving_review_hook_lets_the_run_continue_to_the_next_stage() {
        let (failed, kinds) = review_run_kinds(true);
        assert_eq!(failed, 0);
        assert_eq!(
            kinds,
            [
                "started:coregister",
                "done:coregister",
                "started:normalize",
                "done:normalize",
                "completed:2/0/0"
            ]
        );
    }

    #[test]
    fn rejecting_review_hook_stops_the_run_awaiting_qc() {
        let (failed, kinds) = review_run_kinds(false);
        assert_eq!(failed, 1);
        assert_eq!(
            kinds,
            [
                "started:coregister",
                "awaiting:coregister",
                "failed:coregister",
                "completed:0/1/0"
            ]
        );
    }

    fn run_fake(root: &Path, stages: Vec<Stage>, plan: bool) -> Vec<RunEvent> {
        let opts = RunOptions {
            stages,
            plan_only: plan,
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            ..RunOptions::new(root, "1")
        };
        let mut events = Vec::new();
        run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, &completing);
        events
    }

    fn lock_dir(root: &Path) -> PathBuf {
        let d = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn lock_json(dir: &Path, stage: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{stage}.json"))).unwrap())
            .unwrap()
    }

    fn invalidation_lines(events: &[RunEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                RunEvent::LogLine { line }
                    if line.contains("approvals invalidated") || line.contains("reviews reset") =>
                {
                    Some(line.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn completed_coreg_invalidates_approved_dependents() {
        let root = scratch("inval_coreg");
        let locks = lock_dir(&root);
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let anchor = sub.preop_anchor_raw.clone().unwrap();
        locks::write_approval(
            &locks.join("normalize.json"),
            true,
            Some("ants_syn"),
            std::slice::from_ref(&anchor),
            Some(1.0),
        )
        .unwrap();
        std::fs::write(locks.join("reconstruct.json"), r#"{"approval": true}"#).unwrap();
        std::fs::write(locks.join("vat.json"), r#"{"approval": false}"#).unwrap();
        std::fs::write(locks.join("orientation.json"), "{not json").unwrap();
        let events = run_fake(&root, vec![Stage::Coregister], false);
        let n = lock_json(&locks, "normalize");
        assert_eq!(n["approval"], json!(false));
        assert_eq!(n["invalidated_by"], json!("coreg"));
        assert!(n["invalidated_at"].as_f64().unwrap() > 0.0);
        let keys: Vec<&String> = n.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "approval",
                "method",
                "inputs",
                "timestamp",
                "invalidated_by",
                "invalidated_at"
            ]
        );
        assert_eq!(lock_json(&locks, "reconstruct")["approval"], json!(false));
        assert_eq!(
            lock_json(&locks, "reconstruct")["invalidated_by"],
            json!("coreg")
        );
        assert_eq!(
            std::fs::read_to_string(locks.join("vat.json")).unwrap(),
            r#"{"approval": false}"#
        );
        assert_eq!(
            std::fs::read_to_string(locks.join("orientation.json")).unwrap(),
            "{not json"
        );
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: coreg re-ran; approvals invalidated: normalize, reconstruct"]
        );
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.starts_with("dbsloc: warning: corrupt lock")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stages_after_a_completed_one_rerun_in_the_same_run() {
        let root = scratch("inval_same_run");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        let events = run_fake(&root, vec![Stage::Coregister, Stage::Normalize], false);
        assert_eq!(
            outcomes(&events),
            vec![
                ("coregister".to_owned(), "ok:fake".to_owned()),
                ("normalize".to_owned(), "ok:fake".to_owned())
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skipped_approved_coreg_changes_no_other_lock() {
        let root = scratch("inval_skip");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("coreg.json"), r#"{"approval": true}"#).unwrap();
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        let events = run_fake(&root, vec![Stage::Coregister], false);
        assert_eq!(outcomes(&events)[0].1, "skip:approved");
        assert_eq!(
            std::fs::read_to_string(locks.join("normalize.json")).unwrap(),
            r#"{"approval": true}"#
        );
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn run_one(
        root: &Path,
        stage: Stage,
        overwrite: bool,
        checkreg: bool,
        runner: &StageRunner,
    ) -> Vec<RunEvent> {
        let opts = RunOptions {
            stages: vec![stage],
            overwrite_approved: overwrite,
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg,
            ..RunOptions::new(root, "1")
        };
        let mut events = Vec::new();
        run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, runner);
        events
    }

    fn pending_runner(
        _: &RunOptions,
        stage: Stage,
        _: Option<&mpsc::Sender<RunEvent>>,
        _: Option<&AtomicBool>,
    ) -> error::PipelineResult<StageOutcome> {
        Err(error::PipelineError::QcPending {
            stage: stage.cli_name().to_owned(),
            manifest: "approve in the viewer".to_owned(),
        })
    }

    fn withdrawn_line(name: &str) -> String {
        format!(
            "dbsloc: {name} runs again; its approval is withdrawn until the new result is approved"
        )
    }

    fn line_at(events: &[RunEvent], wanted: &str) -> Option<usize> {
        events
            .iter()
            .position(|e| matches!(e, RunEvent::LogLine { line } if line == wanted))
    }

    fn write_review_log(o: &RunOptions, stage: Stage, approve: Option<&str>) {
        let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
        let mlog = review_log(&sub, stage).unwrap();
        std::fs::create_dir_all(mlog.parent().unwrap()).unwrap();
        std::fs::write(&mlog, r#"{"approval": 0}"#).unwrap();
        if let Some(branch) = approve {
            checkreg::apply_approval(&mlog, branch, None, true).unwrap();
        }
    }

    fn approve_with_inputs(root: &Path, stage: &str) {
        let sub = resolve_subject_with(root, "1", false).unwrap();
        let anchor = sub.preop_anchor_raw.clone().unwrap();
        locks::write_approval(
            &lock_dir(root).join(format!("{stage}.json")),
            true,
            Some("user"),
            std::slice::from_ref(&anchor),
            Some(1.0),
        )
        .unwrap();
    }

    fn a_rerun_awaiting_review_withdraws_its_own_approval(stage: Stage, checkreg: bool) {
        let name = stage.cli_name();
        let root = scratch(&format!("own_lock_{name}_{checkreg}"));
        let locks = lock_dir(&root);
        approve_with_inputs(&root, name);
        let events = run_one(&root, stage, false, checkreg, &completing);
        assert_eq!(outcomes(&events)[0].1, "skip:approved");
        let events = run_one(&root, stage, true, checkreg, &pending_runner);
        let awaiting = events
            .iter()
            .position(|e| matches!(e, RunEvent::StageAwaitingQc { .. }))
            .unwrap();
        assert!(line_at(&events, &withdrawn_line(name)).unwrap() < awaiting);
        let lock = lock_json(&locks, name);
        assert_eq!(lock["approval"], json!(false));
        assert_eq!(lock["invalidated_by"], json!(name));
        assert_eq!(lock["method"], json!("user"));
        assert!(lock["invalidated_at"].as_f64().unwrap() > 0.0);
        assert!(invalidation_lines(&events).is_empty());

        let events = run_one(&root, stage, false, checkreg, &completing);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_normalize_rerun_awaiting_review_withdraws_its_own_approval() {
        a_rerun_awaiting_review_withdraws_its_own_approval(Stage::Normalize, false);
    }

    #[test]
    fn a_coreg_rerun_awaiting_review_with_the_gate_on_withdraws_its_own_approval() {
        a_rerun_awaiting_review_withdraws_its_own_approval(Stage::Coregister, true);
    }

    #[test]
    fn a_brainshift_rerun_awaiting_review_withdraws_its_own_approval() {
        a_rerun_awaiting_review_withdraws_its_own_approval(Stage::Brainshift, true);
    }

    fn a_completed_rerun_withdraws_its_own_approval(stage: Stage) {
        let name = stage.cli_name();
        let root = scratch(&format!("own_lock_done_{name}"));
        let locks = lock_dir(&root);
        std::fs::write(locks.join(format!("{name}.json")), r#"{"approval": true}"#).unwrap();
        let events = run_one(&root, stage, true, false, &completing);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        let lock = lock_json(&locks, name);
        assert_eq!(lock["approval"], json!(false));
        assert_eq!(lock["invalidated_by"], json!(name));
        assert!(line_at(&events, &withdrawn_line(name)).is_some());
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_completed_preprocess_rerun_withdraws_its_own_approval() {
        a_completed_rerun_withdraws_its_own_approval(Stage::Preprocess);
    }

    #[test]
    fn a_completed_precoreg_rerun_withdraws_its_own_approval() {
        a_completed_rerun_withdraws_its_own_approval(Stage::Precoreg);
    }

    #[test]
    fn a_completed_coreg_without_review_withdraws_its_own_approval_and_its_dependents() {
        let root = scratch("own_lock_coreg_done");
        let locks = lock_dir(&root);
        for stage in ["coreg", "normalize"] {
            std::fs::write(locks.join(format!("{stage}.json")), r#"{"approval": true}"#).unwrap();
        }
        let events = run_one(&root, Stage::Coregister, true, false, &completing);
        for stage in ["coreg", "normalize"] {
            let lock = lock_json(&locks, stage);
            assert_eq!(lock["approval"], json!(false), "{stage}");
            assert_eq!(lock["invalidated_by"], json!("coreg"), "{stage}");
        }
        assert!(line_at(&events, &withdrawn_line("coreg")).is_some());
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: coreg re-ran; approvals invalidated: normalize"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_completed_rerun_with_the_gate_on_but_an_unapproved_log_withdraws_its_approval() {
        let root = scratch("own_lock_log_zero");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        let unreviewed = |o: &RunOptions,
                          stage: Stage,
                          _: Option<&mpsc::Sender<RunEvent>>,
                          _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            write_review_log(o, stage, None);
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(&root, Stage::Normalize, true, true, &unreviewed);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        assert_eq!(lock_json(&locks, "normalize")["approval"], json!(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn a_rerun_approved_in_its_review_keeps_its_approval(stage: Stage, branch: &'static str) {
        let name = stage.cli_name();
        let root = scratch(&format!("own_lock_reviewed_{name}"));
        let locks = lock_dir(&root);
        std::fs::write(locks.join(format!("{name}.json")), r#"{"approval": true}"#).unwrap();
        let reviewed = move |o: &RunOptions,
                             stage: Stage,
                             _: Option<&mpsc::Sender<RunEvent>>,
                             _: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            write_review_log(o, stage, Some(branch));
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(&root, stage, true, true, &reviewed);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        assert_eq!(
            std::fs::read_to_string(locks.join(format!("{name}.json"))).unwrap(),
            r#"{"approval": true}"#
        );
        let kept = format!(
            "dbsloc: note: {name}: the new result was approved in its review during this run; its approval is kept"
        );
        assert!(
            line_at(&events, &withdrawn_line(name)).unwrap() < line_at(&events, &kept).unwrap()
        );
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_normalize_rerun_approved_in_its_review_during_the_run_keeps_its_approval() {
        a_rerun_approved_in_its_review_keeps_its_approval(Stage::Normalize, "norm");
    }

    #[test]
    fn a_brainshift_rerun_approved_in_its_review_during_the_run_keeps_its_approval() {
        a_rerun_approved_in_its_review_keeps_its_approval(Stage::Brainshift, "brainshift");
    }

    fn a_failure_after_the_commit_invalidates_the_stage_and_its_dependents(
        stage: Stage,
        message: &'static str,
    ) {
        let name = stage.cli_name();
        let root = scratch(&format!("own_lock_fail_{name}"));
        let locks = lock_dir(&root);
        for s in [name, "reconstruct"] {
            std::fs::write(locks.join(format!("{s}.json")), r#"{"approval": true}"#).unwrap();
        }
        let committed_then_failed = move |o: &RunOptions,
                                          stage: Stage,
                                          _: Option<&mpsc::Sender<RunEvent>>,
                                          _: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            write_review_log(o, stage, None);
            Err(error::PipelineError::Message(message.to_owned()))
        };
        let events = run_one(&root, stage, true, true, &committed_then_failed);
        assert!(outcomes(&events)[0].1.contains(message), "{events:?}");
        assert_eq!(lock_json(&locks, name)["approval"], json!(false));
        let reco = lock_json(&locks, "reconstruct");
        assert_eq!(reco["approval"], json!(false));
        assert_eq!(reco["invalidated_by"], json!(name));
        assert_eq!(
            invalidation_lines(&events),
            vec![format!(
                "dbsloc: {name} re-ran; approvals invalidated: reconstruct"
            )]
        );
        let events = run_one(&root, stage, false, true, &completing);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_review_cancelled_after_normalize_committed_invalidates_it_and_its_dependents() {
        a_failure_after_the_commit_invalidates_the_stage_and_its_dependents(
            Stage::Normalize,
            "cancelled by user",
        );
    }

    #[test]
    fn a_brainshift_gate_error_after_the_commit_invalidates_it_and_its_dependents() {
        a_failure_after_the_commit_invalidates_the_stage_and_its_dependents(
            Stage::Brainshift,
            "brainshift: checkreg needs a display",
        );
    }

    fn lock_reads_withdrawn(o: &RunOptions, stage: Stage) {
        let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
        let lock = locks::read_approval(&stage_approval_path(&sub.locks_dir(), stage.cli_name()))
            .unwrap()
            .unwrap();
        assert_eq!(lock["approval"], json!(false), "during the body");
        assert_eq!(lock["invalidated_by"], json!(stage.cli_name()));
    }

    #[test]
    fn a_failure_before_the_commit_keeps_the_approval_and_the_dependents() {
        let root = scratch("own_lock_fail_early");
        let locks = lock_dir(&root);
        for s in ["normalize", "reconstruct"] {
            std::fs::write(locks.join(format!("{s}.json")), r#"{"approval": true}"#).unwrap();
        }
        let failing = |o: &RunOptions,
                       stage: Stage,
                       _: Option<&mpsc::Sender<RunEvent>>,
                       _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            lock_reads_withdrawn(o, stage);
            Err(error::PipelineError::Message(
                "antsRegistration failed with 1".to_owned(),
            ))
        };
        let events = run_one(&root, Stage::Normalize, true, false, &failing);
        for s in ["normalize", "reconstruct"] {
            assert_eq!(
                std::fs::read_to_string(locks.join(format!("{s}.json"))).unwrap(),
                r#"{"approval": true}"#,
                "{s}"
            );
        }
        assert!(line_at(
            &events,
            "dbsloc: note: normalize: the step stopped before it replaced any output; its approval is kept"
        )
        .is_some());
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_prerequisite_keeps_the_approval_byte_for_byte() {
        let root = scratch("own_lock_prereq");
        let locks = lock_dir(&root);
        for s in ["normalize", "reconstruct"] {
            std::fs::write(locks.join(format!("{s}.json")), r#"{"approval": true}"#).unwrap();
        }
        let opts = RunOptions {
            stages: vec![Stage::Normalize],
            overwrite_approved: true,
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: false,
            reg_backend: reg_backend::RegBackend::Ants,
            normalization_template: Some(root.join("no_such_template.nii.gz")),
            ..RunOptions::new(&root, "1")
        };
        let events = run_pipeline_collect(&opts).1;
        assert!(
            outcomes(&events)[0].1.starts_with("error: normalize: "),
            "{events:?}"
        );
        for s in ["normalize", "reconstruct"] {
            assert_eq!(
                std::fs::read_to_string(locks.join(format!("{s}.json"))).unwrap(),
                r#"{"approval": true}"#,
                "{s}"
            );
        }
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stage_body_that_skips_restores_the_approval_byte_for_byte() {
        let root = scratch("own_lock_body_skip");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("brainshift.json"), r#"{"approval": true}"#).unwrap();
        let stub = |o: &RunOptions,
                    stage: Stage,
                    _: Option<&mpsc::Sender<RunEvent>>,
                    _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            lock_reads_withdrawn(o, stage);
            Ok(StageOutcome::skipped_as("not implemented", "skip:stub"))
        };
        let events = run_one(&root, Stage::Brainshift, true, true, &stub);
        assert_eq!(outcomes(&events)[0].1, "skip:stub");
        assert_eq!(
            std::fs::read_to_string(locks.join("brainshift.json")).unwrap(),
            r#"{"approval": true}"#
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_body_that_replaced_outputs_and_then_skipped_invalidates_like_a_failure() {
        let root = scratch("own_lock_skip_after_write");
        let locks = lock_dir(&root);
        for s in ["normalize", "reconstruct"] {
            std::fs::write(locks.join(format!("{s}.json")), r#"{"approval": true}"#).unwrap();
        }
        let body = |o: &RunOptions,
                    _: Stage,
                    _: Option<&mpsc::Sender<RunEvent>>,
                    _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
            outputs::write_replacing(&sub.norm_dir.join("replaced.nii"), b"new").unwrap();
            Ok(StageOutcome::skipped_as("nothing left to do", "skip:late"))
        };
        let events = run_one(&root, Stage::Normalize, true, false, &body);
        assert_eq!(outcomes(&events)[0].1, "skip:late");
        assert_eq!(lock_json(&locks, "normalize")["approval"], json!(false));
        let reco = lock_json(&locks, "reconstruct");
        assert_eq!(reco["approval"], json!(false));
        assert_eq!(reco["invalidated_by"], json!("normalize"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_approval_that_cannot_be_withdrawn_stops_the_stage_before_its_body() {
        let root = scratch("own_lock_unwritable");
        let locks = lock_dir(&root);
        let lock = locks.join("normalize.json");
        std::fs::write(&lock, r#"{"approval": true}"#).unwrap();
        std::fs::create_dir_all(outputs::partial_of(&lock).join("blocker")).unwrap();
        let ran = std::sync::Arc::new(AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&ran);
        let body = move |_: &RunOptions,
                         _: Stage,
                         _: Option<&mpsc::Sender<RunEvent>>,
                         _: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            seen.store(true, Ordering::SeqCst);
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(&root, Stage::Normalize, true, false, &body);
        assert!(!ran.load(Ordering::SeqCst));
        let result = &outcomes(&events)[0].1;
        assert!(
            result.starts_with(
                "error: normalize: cannot withdraw its approval before it runs again: "
            ),
            "{result}"
        );
        assert_eq!(
            std::fs::read_to_string(&lock).unwrap(),
            r#"{"approval": true}"#
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_commit_that_fails_part_way_invalidates_the_dependents() {
        let root = scratch("own_lock_partial");
        let locks = lock_dir(&root);
        for s in ["normalize", "reconstruct"] {
            std::fs::write(locks.join(format!("{s}.json")), r#"{"approval": true}"#).unwrap();
        }
        let partial = |o: &RunOptions,
                       _: Stage,
                       _: Option<&mpsc::Sender<RunEvent>>,
                       _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
            let dir = sub.norm_dir.join("partial_commit");
            std::fs::create_dir_all(&dir).unwrap();
            let first = dir.join("first.nii");
            let second = dir.join("second.nii");
            std::fs::create_dir_all(second.join("in_the_way")).unwrap();
            let staged = outputs::Staged::new(vec![first.clone(), second.clone()]).unwrap();
            std::fs::write(staged.tmp(&first), b"new").unwrap();
            std::fs::write(staged.tmp(&second), b"new").unwrap();
            staged.finish(Ok(()), None)?;
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(&root, Stage::Normalize, true, false, &partial);
        assert!(outcomes(&events)[0].1.starts_with("error: "), "{events:?}");
        assert_eq!(lock_json(&locks, "normalize")["approval"], json!(false));
        let reco = lock_json(&locks, "reconstruct");
        assert_eq!(reco["approval"], json!(false));
        assert_eq!(reco["invalidated_by"], json!("normalize"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_restore_leaves_a_lock_written_during_the_run_alone() {
        let root = scratch("own_lock_newer");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        let newer = r#"{"approval": true, "method": "server"}"#;
        let body = move |o: &RunOptions,
                         stage: Stage,
                         _: Option<&mpsc::Sender<RunEvent>>,
                         _: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            write_review_log(o, stage, Some("norm"));
            let sub = resolve_subject_with(&o.bids_dir, &o.subject, false).unwrap();
            std::fs::write(stage_approval_path(&sub.locks_dir(), "normalize"), newer).unwrap();
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(&root, Stage::Normalize, true, true, &body);
        assert_eq!(
            std::fs::read_to_string(locks.join("normalize.json")).unwrap(),
            newer
        );
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("changed while the step ran")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn completed_with_log(
        root: &Path,
        stage: Stage,
        before: Option<&str>,
        body_log: Option<&'static str>,
    ) -> Value {
        let name = stage.cli_name();
        let locks = lock_dir(root);
        std::fs::write(locks.join(format!("{name}.json")), r#"{"approval": true}"#).unwrap();
        let sub = resolve_subject_with(root, "1", false).unwrap();
        let mlog = review_log(&sub, stage).unwrap();
        std::fs::create_dir_all(mlog.parent().unwrap()).unwrap();
        if let Some(text) = before {
            std::fs::write(&mlog, text).unwrap();
        }
        let body = move |_: &RunOptions,
                         _: Stage,
                         _: Option<&mpsc::Sender<RunEvent>>,
                         _: Option<&AtomicBool>|
              -> error::PipelineResult<StageOutcome> {
            if let Some(text) = body_log {
                std::fs::write(&mlog, text).unwrap();
            }
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let events = run_one(root, stage, true, true, &body);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        lock_json(&locks, name)
    }

    #[test]
    fn a_coreg_log_with_one_unapproved_modality_does_not_restore() {
        let root = scratch("own_lock_coreg_dict");
        let lock = completed_with_log(
            &root,
            Stage::Coregister,
            None,
            Some(r#"{"approval": {"CT": 1, "ax_MRI": 0}}"#),
        );
        assert_eq!(lock["approval"], json!(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_approving_log_not_rewritten_in_this_attempt_does_not_restore() {
        let root = scratch("own_lock_old_log");
        let lock = completed_with_log(&root, Stage::Normalize, Some(r#"{"approval": 1}"#), None);
        assert_eq!(lock["approval"], json!(false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_reviewed_restore_with_fingerprints_records_the_inputs_of_this_run() {
        let root = scratch("own_lock_fingerprint");
        let locks = lock_dir(&root);
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let anchor = sub.preop_anchor_raw.clone().unwrap();
        locks::write_approval(
            &locks.join("normalize.json"),
            true,
            Some("user"),
            std::slice::from_ref(&anchor),
            Some(1.0),
        )
        .unwrap();
        std::fs::write(&anchor, b"changed anchor").unwrap();
        let body = |o: &RunOptions,
                    stage: Stage,
                    _: Option<&mpsc::Sender<RunEvent>>,
                    _: Option<&AtomicBool>|
         -> error::PipelineResult<StageOutcome> {
            write_review_log(o, stage, Some("norm"));
            Ok(StageOutcome::Completed("ok:fake".into()))
        };
        let opts = RunOptions {
            stages: vec![Stage::Normalize],
            stop_on_error: false,
            headless: true,
            no_scene3d: true,
            checkreg: true,
            fingerprint_locks: true,
            ..RunOptions::new(&root, "1")
        };
        let mut events = Vec::new();
        run_pipeline_with_runner(&opts, |e| events.push(e.clone()), None, None, &body);
        assert_eq!(outcomes(&events)[0].1, "ok:fake");
        let lock = lock_json(&locks, "normalize");
        assert_eq!(lock["approval"], json!(true));
        assert_eq!(lock["method"], json!("user"));
        let paths = default_lock_input_paths(&resolve_subject_with(&root, "1", false).unwrap())
            .remove("normalize")
            .unwrap();
        assert!(locks::lock_allows_skip(&locks.join("normalize.json"), Some(&paths)).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rerun_leaves_a_missing_own_lock_absent() {
        let root = scratch("own_lock_absent");
        let locks = lock_dir(&root);
        let events = run_one(&root, Stage::Normalize, false, false, &pending_runner);
        assert!(!locks.join("normalize.json").exists());
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_skipped_stage_keeps_its_own_lock_byte_for_byte() {
        let root = scratch("own_lock_skip");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("preprocess.json"), r#"{"approval": true}"#).unwrap();
        let events = run_one(&root, Stage::Preprocess, false, false, &completing);
        assert_eq!(outcomes(&events)[0].1, "skip:approved");
        assert_eq!(
            std::fs::read_to_string(locks.join("preprocess.json")).unwrap(),
            r#"{"approval": true}"#
        );
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_changed_prints_no_line() {
        let root = scratch("inval_none");
        let events = run_fake(&root, vec![Stage::Preprocess], false);
        assert!(invalidation_lines(&events).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_only_invalidates_nothing() {
        let root = scratch("inval_plan");
        let locks = lock_dir(&root);
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        let events = run_fake(&root, vec![Stage::Preprocess], true);
        assert!(invalidation_lines(&events).is_empty());
        assert_eq!(
            std::fs::read_to_string(locks.join("normalize.json")).unwrap(),
            r#"{"approval": true}"#
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn completed_coreg_clears_the_normalize_and_brainshift_method_log_approval() {
        let root = scratch("inval_mlog");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let norm = checkreg::default_norm_method_log_path(&sub);
        let shift = checkreg::default_brainshift_method_log_path(&sub);
        checkreg::apply_approval(&norm, "norm", None, true).unwrap();
        checkreg::apply_approval(&shift, "brainshift", None, true).unwrap();
        assert!(checkreg::method_log_allows_continue(&norm));
        let events = run_fake(&root, vec![Stage::Coregister], false);
        assert!(!checkreg::method_log_allows_continue(&norm));
        assert!(!checkreg::method_log_allows_continue(&shift));
        for log in [&norm, &shift] {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(log).unwrap()).unwrap();
            assert_eq!(v["approval"], json!(0));
            assert!(v.get("reviewed").is_none());
            assert_eq!(
                checkreg::review_state(log),
                checkreg::ReviewState::NotReviewed
            );
        }
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: coreg re-ran; approvals invalidated: normalize, brainshift"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rejected_method_log_loses_its_marker_when_the_upstream_step_reruns() {
        let root = scratch("inval_rejected");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let norm = checkreg::default_norm_method_log_path(&sub);
        checkreg::apply_approval(&norm, "norm", None, false).unwrap();
        assert_eq!(
            checkreg::review_state(&norm),
            checkreg::ReviewState::Rejected
        );
        let _ = run_fake(&root, vec![Stage::Coregister], false);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&norm).unwrap()).unwrap();
        assert_eq!(v["approval"], json!(0));
        assert!(v.get("reviewed").is_none());
        assert_eq!(
            checkreg::review_state(&norm),
            checkreg::ReviewState::NotReviewed
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rejected_method_log_reports_a_review_reset_not_an_invalidated_approval() {
        let root = scratch("inval_rejected_line");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let norm = checkreg::default_norm_method_log_path(&sub);
        checkreg::apply_approval(&norm, "norm", None, false).unwrap();
        let events = run_fake(&root, vec![Stage::Coregister], false);
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: coreg re-ran; reviews reset: normalize"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalidating_coreg_clears_every_modality_in_its_method_log() {
        let root = scratch("inval_coreg_mlog");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let mlog = checkreg::default_coreg_method_log_path(&sub);
        checkreg::apply_approval(&mlog, "coreg", Some("T2w"), true).unwrap();
        checkreg::apply_approval(&mlog, "coreg", Some("postop-CT"), true).unwrap();
        let events = run_fake(&root, vec![Stage::Preprocess], false);
        assert!(!checkreg::slide_approved(&mlog, "coreg", Some("T2w")));
        assert!(!checkreg::slide_approved(&mlog, "coreg", Some("postop-CT")));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&mlog).unwrap()).unwrap();
        assert!(v.get("reviewed").is_none());
        assert_eq!(
            checkreg::review_state(&mlog),
            checkreg::ReviewState::NotReviewed
        );
        assert_eq!(
            invalidation_lines(&events),
            vec!["dbsloc: preprocess re-ran; approvals invalidated: coreg"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_results_use_cli_names() {
        let root = scratch("plan");
        let got = run(&root, vec![Stage::Bids, Stage::Coregister], true, false);
        assert_eq!(
            got,
            vec![
                ("bids".to_owned(), "plan:bids".to_owned()),
                ("coregister".to_owned(), "plan:coreg".to_owned())
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_lock_skips_unless_overwritten() {
        let root = scratch("lock");
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("precoreg.json"), r#"{"approval": true}"#).unwrap();
        let got = run(&root, vec![Stage::Bids, Stage::Precoreg], true, false);
        assert_eq!(got[0].1, "plan:bids");
        assert_eq!(got[1].1, "skip:approved");
        let got = run(&root, vec![Stage::Precoreg], true, true);
        assert_eq!(got[0].1, "plan:precoreg");
        let got = run(
            &root,
            vec![Stage::Precoreg, Stage::Orientation, Stage::Vat],
            false,
            false,
        );
        assert_eq!(got[0].1, "skip:approved");
        assert!(
            got[1]
                .1
                .starts_with("error: orientation: missing Reco JSON: "),
            "{}",
            got[1].1
        );
        assert_eq!(got[2].1, "skip:vat: no stimulation settings");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_chosen_output_root_holds_the_locks_and_is_in_the_plan() {
        let root = scratch("outroot");
        let out = root.join("elsewhere");
        let anchor = root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz");
        let opts = RunOptions {
            stages: vec![Stage::Bids],
            plan_only: true,
            fingerprint_locks: true,
            output_dir: Some(out.clone()),
            ..RunOptions::new(&root, "1")
        };
        let plan = plan_run(&opts).unwrap();
        let out_abs = paths::absolute(&out);
        assert_eq!(plan.subject.subj_dir, out_abs.join("sub-1"));
        let j = plan.to_json(&opts);
        assert_eq!(j["output_dir"], json!(out_abs.to_string_lossy()));
        let default = RunOptions::new(&root, "1");
        assert_eq!(
            plan_run(&default).unwrap().to_json(&default)["output_dir"],
            json!(paths::absolute(&root)
                .join("derivatives")
                .join("leaddbs")
                .to_string_lossy())
        );
        let lock = out.join("sub-1/locks/bids.json");
        locks::write_approval(&lock, true, None, std::slice::from_ref(&anchor), None).unwrap();
        assert_eq!(
            outcomes(&run_pipeline_collect(&opts).1)[0].1,
            "skip:approved"
        );
        assert!(!root.join("derivatives").exists());
        assert_eq!(
            outcomes(&run_pipeline_collect(&default).1)[0].1,
            "plan:bids"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_fingerprint_does_not_skip() {
        let root = scratch("stale");
        let anchor = root.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz");
        let lock = root.join("derivatives/leaddbs/sub-1/locks/bids.json");
        locks::write_approval(&lock, true, None, std::slice::from_ref(&anchor), None).unwrap();
        let opts = RunOptions {
            stages: vec![Stage::Bids],
            plan_only: true,
            fingerprint_locks: true,
            ..RunOptions::new(&root, "1")
        };
        assert_eq!(
            outcomes(&run_pipeline_collect(&opts).1)[0].1,
            "skip:approved"
        );
        std::fs::write(&anchor, b"y").unwrap();
        assert_eq!(outcomes(&run_pipeline_collect(&opts).1)[0].1, "plan:bids");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_normalize_reruns_when_the_method_changes() {
        let root = scratch("normmethod");
        let tpl = root.join("tpl").join("t1.nii.gz");
        std::fs::create_dir_all(tpl.parent().unwrap()).unwrap();
        std::fs::write(&tpl, b"x").unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        std::fs::create_dir_all(derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            derivatives::norm_method_log(&sub),
            json!({
                "method": "other_method",
                "channels": ["T1w"],
                "template": [paths::absolute(&tpl).to_string_lossy()],
                "norm_isotropic_mm": 1.0,
            })
            .to_string(),
        )
        .unwrap();
        let opts = RunOptions {
            stages: vec![Stage::Normalize],
            plan_only: true,
            normalization_template: Some(tpl.clone()),
            ..RunOptions::new(&root, "1")
        };
        let events = run_pipeline_collect(&opts).1;
        assert_eq!(outcomes(&events)[0].1, "plan:normalize");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("made with other_method")
                && line.contains("ants_syn")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_normalize_reruns_when_the_registration_backend_differs() {
        let root = scratch("normbackend");
        let tpl = root.join("tpl").join("t1.nii.gz");
        std::fs::create_dir_all(tpl.parent().unwrap()).unwrap();
        std::fs::write(&tpl, b"x").unwrap();
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        std::fs::create_dir_all(derivatives::norm_log_dir(&sub)).unwrap();
        let write_record = |backend: Option<&str>| {
            let mut rec = json!({
                "method": "ants_syn",
                "channels": ["T1w"],
                "template": [paths::absolute(&tpl).to_string_lossy()],
                "norm_isotropic_mm": 1.0,
            });
            if let Some(b) = backend {
                rec["reg_backend"] = json!(b);
            }
            std::fs::write(derivatives::norm_method_log(&sub), rec.to_string()).unwrap();
        };
        let run_with = |backend: reg_backend::RegBackend| {
            let opts = RunOptions {
                stages: vec![Stage::Normalize],
                plan_only: true,
                normalization_template: Some(tpl.clone()),
                reg_backend: backend,
                ..RunOptions::new(&root, "1")
            };
            run_pipeline_collect(&opts).1
        };
        use reg_backend::RegBackend;
        write_record(None);
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Ants))[0].1, "skip:approved");
        let events = run_with(RegBackend::Wgpu);
        assert_eq!(outcomes(&events)[0].1, "plan:normalize");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.starts_with("dbsloc: note: normalize: ")
                && line.contains("made with the ants registration backend, this run asks for wgpu")
                && line.ends_with("; the stage runs")
        )));
        write_record(Some("wgpu"));
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Wgpu))[0].1, "skip:approved");
        let events = run_with(RegBackend::Ants);
        assert_eq!(outcomes(&events)[0].1, "plan:normalize");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("made with the wgpu registration backend")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_normalize_reruns_when_grid_or_template_changed() {
        let root = scratch("normgrid");
        let tpl = root.join("tpl").join("t1.nii.gz");
        let tpl2 = root.join("tpl2").join("t1.nii.gz");
        for t in [&tpl, &tpl2] {
            std::fs::create_dir_all(t.parent().unwrap()).unwrap();
            std::fs::write(t, b"x").unwrap();
        }
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("normalize.json"), r#"{"approval": true}"#).unwrap();
        std::fs::create_dir_all(derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            derivatives::norm_method_log(&sub),
            json!({
                "method": "ants_syn",
                "channels": ["T1w"],
                "template": [paths::absolute(&tpl).to_string_lossy()],
                "norm_isotropic_mm": 1.5,
            })
            .to_string(),
        )
        .unwrap();
        let run_with = |template: &PathBuf, grid: options::NormGrid| {
            let opts = RunOptions {
                stages: vec![Stage::Normalize],
                plan_only: true,
                normalization_template: Some(template.clone()),
                norm_isotropic_mm: grid,
                reg_backend: reg_backend::RegBackend::Ants,
                ..RunOptions::new(&root, "1")
            };
            run_pipeline_collect(&opts).1
        };
        use options::NormGrid;
        assert_eq!(
            outcomes(&run_with(&tpl, NormGrid::Mm(1.5)))[0].1,
            "skip:approved"
        );
        assert_eq!(
            outcomes(&run_with(&tpl, NormGrid::Template))[0].1,
            "skip:approved",
            "a grid that was not chosen never re-runs approved outputs"
        );
        let events = run_with(&tpl, NormGrid::Mm(1.0));
        assert_eq!(outcomes(&events)[0].1, "plan:normalize");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.starts_with("dbsloc: note: normalize: ")
                && line.contains("made on a 1.5 mm grid, this run asks for a 1.0 mm grid")
                && line.ends_with("; the stage runs")
        )));
        let events = run_with(&tpl2, NormGrid::Mm(1.5));
        assert_eq!(outcomes(&events)[0].1, "plan:normalize");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("made for the template")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_coreg_reruns_when_the_registration_backend_differs() {
        let root = scratch("coregbackend");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("coreg.json"), r#"{"approval": true}"#).unwrap();
        let record = derivatives::coreg_multistart_record(&sub);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        let write_record = |backend: Option<&str>| {
            let mut rec = json!({
                "starts": [],
                "chosen_pitch_deg": 0.0,
                "fixed_centroid_lps": [0.0, 0.0, 0.0],
                "moving_centroid_lps": [0.0, 0.0, 0.0],
                "coarse_schedule": {},
                "fixed_mask": "m.nii.gz",
            });
            if let Some(b) = backend {
                rec["reg_backend"] = json!(b);
            }
            std::fs::write(&record, rec.to_string()).unwrap();
        };
        let run_with = |backend: reg_backend::RegBackend| {
            let opts = RunOptions {
                stages: vec![Stage::Coregister],
                plan_only: true,
                reg_backend: backend,
                ..RunOptions::new(&root, "1")
            };
            run_pipeline_collect(&opts).1
        };
        use reg_backend::RegBackend;
        write_record(None);
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Ants))[0].1, "skip:approved");
        let events = run_with(RegBackend::Wgpu);
        assert_eq!(outcomes(&events)[0].1, "plan:coreg");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.starts_with("dbsloc: note: coreg: ")
                && line.contains("made with the ants registration backend, this run asks for wgpu")
                && line.ends_with("; the stage runs")
        )));
        write_record(Some("wgpu"));
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Wgpu))[0].1, "skip:approved");
        let events = run_with(RegBackend::Ants);
        assert_eq!(outcomes(&events)[0].1, "plan:coreg");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approved_preprocess_reruns_when_the_registration_backend_differs() {
        let root = scratch("prebackend");
        let sub = resolve_subject_with(&root, "1", false).unwrap();
        let locks = root.join("derivatives/leaddbs/sub-1/locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(locks.join("preprocess.json"), r#"{"approval": true}"#).unwrap();
        let rec = derivatives::preproc_record(&sub);
        std::fs::create_dir_all(&sub.preproc_dir).unwrap();
        let write_record = |backend: Option<&str>| match backend {
            Some(b) => std::fs::write(&rec, json!({"reg_backend": b}).to_string()).unwrap(),
            None => {
                let _ = std::fs::remove_file(&rec);
            }
        };
        let run_with = |backend: reg_backend::RegBackend| {
            let opts = RunOptions {
                stages: vec![Stage::Preprocess],
                plan_only: true,
                reg_backend: backend,
                ..RunOptions::new(&root, "1")
            };
            run_pipeline_collect(&opts).1
        };
        use reg_backend::RegBackend;
        write_record(None);
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Ants))[0].1, "skip:approved");
        let events = run_with(RegBackend::Wgpu);
        assert_eq!(outcomes(&events)[0].1, "plan:preprocess");
        assert!(events.iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.starts_with("dbsloc: note: preprocess: ")
                && line.contains("made with the ants registration backend, this run asks for wgpu")
                && line.ends_with("; the stage runs")
        )));
        write_record(Some("wgpu"));
        assert_eq!(outcomes(&run_with(RegBackend::Auto))[0].1, "skip:approved");
        assert_eq!(outcomes(&run_with(RegBackend::Wgpu))[0].1, "skip:approved");
        let events = run_with(RegBackend::Ants);
        assert_eq!(outcomes(&events)[0].1, "plan:preprocess");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn executed_stub_stages_report_python_tokens() {
        let root = scratch("stubs");
        let got = run(&root, vec![Stage::Bids, Stage::Brainshift], false, false);
        assert_eq!(got[0].1, "plan:bids");
        assert!(
            got[1]
                .1
                .starts_with("error: brainshift: no normalization/anat/"),
            "{got:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolvable_subject_fails_each_stage_when_executing() {
        let root = scratch("nosub");
        let opts = RunOptions {
            stages: vec![Stage::Bids, Stage::Preprocess],
            stop_on_error: true,
            ..RunOptions::new(&root, "9")
        };
        let (failed, events) = run_pipeline_collect(&opts);
        assert_eq!(failed, 1);
        let got = outcomes(&events);
        assert_eq!(got.len(), 1);
        assert!(got[0].1.contains("E_NO_SUBJ"), "{got:?}");
        let plan = plan_run(&opts).unwrap_err();
        assert_eq!(plan.code, bids::E_NO_SUBJ);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_json_lists_cli_stage_names_and_lock_inputs() {
        let root = scratch("planjson");
        let opts = RunOptions {
            stages: vec![Stage::Coregister, Stage::Normalize],
            fingerprint_locks: true,
            normalization_template: Some(root.join("tpl.nii.gz")),
            ..RunOptions::new(&root, "sub-1")
        };
        let p = plan_run(&opts).unwrap();
        let j = p.to_json(&opts);
        assert_eq!(j["subj_id"], json!("1"));
        assert_eq!(j["stages"], json!(["coreg", "normalize"]));
        assert!(j["template"].as_str().unwrap().ends_with("tpl.nii.gz"));
        assert!(j["lock_input_paths"]["normalize"].is_array());
        assert!(j["locks_dir"].as_str().unwrap().ends_with("locks"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn qc_pending_emits_awaiting_qc_then_stage_failed_and_counts_one_failure() {
        let mut events: Vec<RunEvent> = Vec::new();
        let failed = emit_stage_failure(
            &mut |ev| events.push(ev.clone()),
            Stage::Normalize,
            &error::PipelineError::QcPending {
                stage: "normalize".to_owned(),
                manifest: "m".to_owned(),
            },
        );
        assert_eq!(failed, 1);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(&events[0], RunEvent::StageAwaitingQc { stage } if stage == "normalize"));
        assert!(matches!(
            &events[1],
            RunEvent::StageFailed { stage, message }
                if stage == "normalize" && message == "normalize: qc not approved — m"
        ));
    }
}
