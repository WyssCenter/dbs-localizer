// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Run queue: worker threads, `dbsloc_pipeline_runner` subprocess per job, NDJSON event stream,
//! SSE replay, cancel and QC resume.

use std::convert::Infallible;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Weak};
use std::time::Duration;

use axum::response::sse::Event;
use dbsloc_api::{RunRequest, SSE_END_EVENT};
use dbsloc_pipeline::bids::{parse_subj_id, resolve_subject_with};
use dbsloc_pipeline::options::{NormMethod, RunOptions, NORM_METHOD_ANTS_SYN};
use dbsloc_pipeline::protocol::RunEvent;
use dbsloc_pipeline::reg_backend::RegBackend;
use dbsloc_pipeline::run_pipeline_subprocess_cmd;
use dbsloc_pipeline::stages::Stage;
use futures::stream::BoxStream;
use futures::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::audit::AuditEntry;
use crate::db::{JobRow, NewJob};
use crate::error::{ServerErr, E_BAD_REQUEST, E_RUN_ACTIVE, E_SERVER};
use crate::sanitize::Sanitizer;
use crate::state::{AppState, AppStateInner, BusItem};

pub use dbsloc_protocol::NORM_ISOTROPIC_MM_RANGE;

/// Reconstruction methods the pipeline accepts (the `dbsloc run --recon-method` choices).
pub const RECON_METHODS: &[&str] = if dbsloc_pipeline::PACER_AVAILABLE {
    &["auto", "trac_core", "trac_core_native", "pacer"]
} else {
    &["auto", "trac_core", "trac_core_native"]
};
pub const E_PACER_UNAVAILABLE: &str = "E_RECON_pacer_unavailable";

pub const JOB_ENV_KEEP: [&str; 10] = [
    "DBSLOC_ANTS_DIR",
    "DBSLOC_DCM2NIIX_BIN",
    "DBSLOC_REPO_ROOT",
    "DBSLOC_DATA_DIR",
    "DBSLOC_TEMPLATE_T1",
    "DBSLOC_TEMPLATE_T2",
    "DBSLOC_GUI_BIN",
    "DBSLOC_SCENE_ATLAS_LEAD_DIR",
    "DBSLOC_SCENE_ATLAS_OBJ_DIR",
    "DBSLOC_SCENE_ATLAS_NII",
];

pub const JOB_ENV_KEEP_PREFIX: &str = "DBSLOC_GPU_";

pub fn job_env_kept(name: &str) -> bool {
    let upper;
    let name = if cfg!(windows) {
        upper = name.to_ascii_uppercase();
        upper.as_str()
    } else {
        name
    };
    !name.starts_with("DBSLOC_")
        || name.starts_with(JOB_ENV_KEEP_PREFIX)
        || JOB_ENV_KEEP.contains(&name)
}

pub fn runner_command<I, K>(bin: &str, server_env: I) -> std::process::Command
where
    I: IntoIterator<Item = K>,
    K: AsRef<std::ffi::OsStr>,
{
    let mut cmd = std::process::Command::new(bin);
    for name in server_env {
        let name = name.as_ref();
        if name.to_str().is_some_and(|n| !job_env_kept(n)) {
            cmd.env_remove(name);
        }
    }
    cmd
}

const WORKER_POLL: Duration = Duration::from_millis(120);
const WORKER_ERROR_BACKOFF: Duration = Duration::from_millis(500);

/// Monotonic-ish opaque id from the current time, process id and a counter (sha256 hex prefix).
pub fn new_id() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut h = dbsloc_pipeline::sha256::Sha256::new();
    h.update(&crate::timeutil::now_unix_millis().to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&n.to_le_bytes());
    let hex = dbsloc_pipeline::sha256::hex(&h.finish());
    hex[..32].to_owned()
}

/// The canonical subject id of a `{sid}` path segment: trimmed, without a `sub-` prefix, and a
/// valid BIDS label (`01`, ` 01` and `sub-01` are one subject).
pub fn canonical_subject(raw: &str) -> Result<String, ServerErr> {
    let trimmed = raw.trim();
    let bare = trimmed.strip_prefix("sub-").unwrap_or(trimmed);
    Ok(parse_subj_id(&format!("sub-{bare}"))?)
}

/// Whether a job phase still has a live event bus.
fn is_live_phase(phase: &str) -> bool {
    matches!(phase, "queued" | "running")
}

/// The canonical stage label of a QC gate or checkreg branch: the legacy gate prefixes `norm` /
/// `coreg` and the driver labels all map to `Stage::label()`.
pub fn gate_label(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "norm" | "normalization" | "normalize" => "normalize".to_owned(),
        "coreg" | "coregister" | "coregistration" => "coregister".to_owned(),
        other => other.to_owned(),
    }
}

/// Whether a checkreg slide `branch` belongs to the QC gate of stage `gated`.
pub fn branch_matches_gate(branch: &str, gated: &str) -> bool {
    gate_label(branch) == gate_label(gated)
}

/// A run request with the `vat` stage fields that `dbsloc_api::RunRequest` does not carry yet.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RunRequestExt {
    /// The client-settable subset shared with `dbsloc-api`.
    #[serde(flatten)]
    pub base: RunRequest,
    /// `auto`, `wgpu` or `cpu`: the engine of the `vat` stage.
    #[serde(default)]
    pub vat_backend: Option<String>,
    /// Stimulation labels the `vat` stage computes; empty = every label of the subject.
    #[serde(default)]
    pub vat_labels: Option<Vec<String>>,
    /// Atlas set of the `vat` overlap statistics.
    #[serde(default)]
    pub vat_stats_atlas: Option<String>,
}

impl From<RunRequest> for RunRequestExt {
    fn from(base: RunRequest) -> Self {
        RunRequestExt {
            base,
            ..RunRequestExt::default()
        }
    }
}

fn apply_vat_fields(opts: &mut RunOptions, ext: &RunRequestExt) -> Result<(), ServerErr> {
    let bad = |m: String| ServerErr::bad_request(E_BAD_REQUEST, m);
    if let Some(b) = &ext.vat_backend {
        opts.vat_backend = dbsloc_pipeline::options::VatBackend::parse(b).map_err(bad)?;
    }
    if let Some(labels) = &ext.vat_labels {
        for l in labels {
            if !dbsloc_pipeline::vat::settings::label_is_valid(l) {
                return Err(bad(format!("vat label {l:?} is not a valid label")));
            }
            if l.starts_with("gs_") {
                return Err(bad(format!(
                    "vat label {l:?}: the prefix gs_ is reserved for group stimulations"
                )));
            }
        }
        opts.vat_labels.clone_from(labels);
    }
    if let Some(a) = &ext.vat_stats_atlas {
        if a.trim().is_empty()
            || a.chars().count() > 128
            || a.chars().any(char::is_control)
            || a.contains(['/', '\\'])
            || a.contains("..")
        {
            return Err(bad(format!(
                "vat_stats_atlas {a:?} is not an atlas set name"
            )));
        }
        opts.vat_stats_atlas = Some(a.clone());
    }
    Ok(())
}

/// Build the server-owned `RunOptions` for a request, then apply and validate the client fields.
pub fn build_run_options(
    state: &AppState,
    dataset: &str,
    subject: &str,
    req: &RunRequest,
) -> Result<RunOptions, ServerErr> {
    build_run_options_ext(state, dataset, subject, &RunRequestExt::from(req.clone()))
}

/// [`build_run_options`] for a request that also carries the `vat` fields.
pub fn build_run_options_ext(
    state: &AppState,
    dataset: &str,
    subject: &str,
    ext: &RunRequestExt,
) -> Result<RunOptions, ServerErr> {
    let req = &ext.base;
    let mut opts = RunOptions::new(state.dataset_root(dataset), subject);
    if let Some(t) = state.config().template_t1() {
        opts.normalization_template = Some(t);
    }
    if let Some(t2) = state.config().template_t2() {
        opts.normalization_template_t2 = Some(t2);
    }
    opts.headless = true;
    opts.checkreg = true;
    opts.no_scene3d = true;
    opts.reg_backend = RegBackend::Wgpu;
    req.apply_to(&mut opts);
    apply_vat_fields(&mut opts, ext)?;
    opts.stop_on_error = true;

    if let dbsloc_api::NormGrid::Mm(mm) = opts.norm_isotropic_mm {
        let (lo, hi) = NORM_ISOTROPIC_MM_RANGE;
        if !mm.is_finite() || mm < lo || mm > hi {
            return Err(ServerErr::bad_request(
                E_BAD_REQUEST,
                format!("norm_isotropic_mm must be a finite value in [{lo}, {hi}] mm, got {mm}"),
            ));
        }
    }
    if req.norm_method.is_some() {
        NormMethod::parse(&opts.norm_method).map_err(|e| {
            let code = e.split(':').next().unwrap_or(E_BAD_REQUEST).to_owned();
            ServerErr::bad_request(code, e)
        })?;
        opts.norm_method = NORM_METHOD_ANTS_SYN.to_owned();
    }
    if req.recon_method.is_some() && dbsloc_pipeline::pacer_method_unavailable(&opts.recon_method) {
        return Err(ServerErr::bad_request(
            E_PACER_UNAVAILABLE,
            dbsloc_pipeline::PACER_UNAVAILABLE,
        ));
    }
    if req.recon_method.is_some() && !RECON_METHODS.contains(&opts.recon_method.as_str()) {
        return Err(ServerErr::bad_request(
            E_BAD_REQUEST,
            format!(
                "unknown recon_method {:?} (known: {})",
                opts.recon_method,
                RECON_METHODS.join(", ")
            ),
        ));
    }
    Ok(opts)
}

fn audit_event(
    state: &AppState,
    client: &str,
    dataset: &str,
    subject: &str,
    action: &str,
    detail: String,
) {
    let _ = crate::audit::append(
        &state.audit_log(),
        &AuditEntry::now(
            client,
            Some(dataset.to_owned()),
            Some(subject.to_owned()),
            action,
            Some(detail),
        ),
    );
}

/// Submit a new run for a subject. The subject must resolve, and a second active run for it is
/// rejected atomically with the insert.
pub fn submit_run(
    state: &AppState,
    dataset: &str,
    subject: &str,
    req: &RunRequest,
    client: &str,
) -> Result<String, ServerErr> {
    submit_run_ext(
        state,
        dataset,
        subject,
        &RunRequestExt::from(req.clone()),
        client,
    )
}

/// [`submit_run`] for a request that also carries the `vat` fields.
pub fn submit_run_ext(
    state: &AppState,
    dataset: &str,
    subject: &str,
    req: &RunRequestExt,
    client: &str,
) -> Result<String, ServerErr> {
    let sid = canonical_subject(subject)?;
    resolve_subject_with(&state.dataset_root(dataset), &sid, true)?;
    let opts = build_run_options_ext(state, dataset, &sid, req)?;
    let options = serde_json::to_string(&opts)
        .map_err(|e| ServerErr::internal(format!("serialise options: {e}")))?;
    let id = new_id();
    let blocking = state.db().insert_unless_active(&NewJob {
        id: id.clone(),
        dataset: dataset.to_owned(),
        subject: sid.clone(),
        options,
        priority: 1,
        resume: false,
        gated_stage: None,
        client: Some(client.to_owned()),
    })?;
    if blocking.is_some() {
        return Err(ServerErr::conflict(
            E_RUN_ACTIVE,
            format!("a run for sub-{sid} is already queued, running or awaiting QC"),
        ));
    }
    state
        .runs()
        .get_or_create(&id, state.run_ndjson(dataset, &id));
    audit_event(
        state,
        client,
        dataset,
        &sid,
        "run_submit",
        format!("id={id}"),
    );
    Ok(id)
}

/// The stages of `stages` that come strictly after `gated`; the whole list when `gated` is absent
/// or not part of it. Accepts both the CLI name and the driver stage label, and the legacy gate
/// prefixes `norm` / `coreg`.
pub fn remaining_stages(stages: &[Stage], gated: Option<&str>) -> Vec<Stage> {
    let Some(gated) = gated.filter(|g| !g.is_empty()) else {
        return stages.to_vec();
    };
    let gated = gate_label(gated);
    match stages
        .iter()
        .position(|s| s.cli_name() == gated || s.label() == gated)
    {
        Some(i) => stages[i + 1..].to_vec(),
        None => stages.to_vec(),
    }
}

/// What approving the last slide of a gate did to the awaiting job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// The job was retired as `resumed`; the run with this id continues after the gate.
    Resumed(String),
    /// The gate was the last stage: the job was retired as `done`.
    Completed,
    /// Another request already claimed the job.
    Lost,
}

/// Claim the awaiting job and submit a resume job for the stages after its gate, atomically.
pub fn submit_resume(
    state: &AppState,
    dataset: &str,
    subject: &str,
    original: &JobRow,
    client: &str,
) -> Result<ResumeOutcome, ServerErr> {
    let mut opts: RunOptions = serde_json::from_str(&original.options)
        .map_err(|e| ServerErr::internal(format!("stored options: {e}")))?;
    let remaining = remaining_stages(&opts.stages, original.gated_stage.as_deref());
    let resume = if remaining.is_empty() {
        None
    } else {
        opts.stages = remaining;
        let options = serde_json::to_string(&opts)
            .map_err(|e| ServerErr::internal(format!("serialise options: {e}")))?;
        Some(NewJob {
            id: new_id(),
            dataset: dataset.to_owned(),
            subject: subject.to_owned(),
            options,
            priority: 0,
            resume: true,
            gated_stage: None,
            client: Some(client.to_owned()),
        })
    };
    if !state.db().retire_awaiting(&original.id, resume.as_ref())? {
        return Ok(ResumeOutcome::Lost);
    }
    finalize_run(state, &original.id);
    match resume {
        Some(job) => {
            state
                .runs()
                .get_or_create(&job.id, state.run_ndjson(dataset, &job.id));
            audit_event(
                state,
                client,
                dataset,
                subject,
                "run_resume",
                format!("id={}", job.id),
            );
            Ok(ResumeOutcome::Resumed(job.id))
        }
        None => Ok(ResumeOutcome::Completed),
    }
}

/// End the awaiting job at its gate because a slide was rejected; `false` when it already left
/// `awaiting_qc`.
pub fn reject_awaiting(state: &AppState, job: &JobRow) -> Result<bool, ServerErr> {
    let stage = job
        .gated_stage
        .as_deref()
        .filter(|s| !s.is_empty())
        .map_or_else(|| "qc".to_owned(), gate_label);
    let changed = state.db().transition(
        &job.id,
        "awaiting_qc",
        "rejected",
        Some(&format!("qc rejected: {stage}")),
    )?;
    if changed {
        finalize_run(state, &job.id);
    }
    Ok(changed)
}

/// Close the live bus of a run with its final status (read from the database) and forget it.
/// Idempotent; safe for a run that has no registry entry.
pub fn finalize_run(state: &AppState, run_id: &str) {
    state.clear_cancel(run_id);
    if let Some(ctx) = state.runs().get(run_id) {
        if let Ok(Some(job)) = state.db().get(run_id) {
            ctx.close(crate::dto::run_status(&job));
        }
        state.runs().remove(run_id);
    }
}

/// Worker thread: claim and run queued jobs until the state is shut down or dropped. The worker
/// holds only a weak handle between jobs, so dropping the last state handle ends it.
pub fn worker_loop(weak: Weak<AppStateInner>) {
    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let state = AppState { inner };
        if state.is_shutdown() {
            return;
        }
        let guard = state.job_guard();
        match state.db().take_next_queued() {
            Ok(Some(job)) => run_guarded(&state, job),
            Ok(None) => {
                drop(guard);
                drop(state);
                std::thread::sleep(WORKER_POLL);
            }
            Err(e) => {
                eprintln!("dbsloc_server: job queue error: {e}");
                drop(guard);
                drop(state);
                std::thread::sleep(WORKER_ERROR_BACKOFF);
            }
        }
    }
}

fn run_guarded(state: &AppState, job: JobRow) {
    let id = job.id.clone();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| run_job(state, job)));
    if outcome.is_err() {
        let _ = state.db().finish(
            &id,
            "failed",
            0,
            1,
            0,
            Some("internal error: the run worker panicked"),
        );
        finalize_run(state, &id);
    }
}

fn run_job(state: &AppState, job: JobRow) {
    if job.subject.starts_with(crate::groups::GROUP_SUBJECT_PREFIX) {
        crate::groups::run_group_job(state, job);
        return;
    }
    let opts: RunOptions = match serde_json::from_str(&job.options) {
        Ok(o) => o,
        Err(e) => {
            let _ = state.db().finish(
                &job.id,
                "failed",
                0,
                1,
                0,
                Some(&format!("invalid stored RunOptions: {e}")),
            );
            finalize_run(state, &job.id);
            return;
        }
    };

    let ndjson = state.run_ndjson(&job.dataset, &job.id);
    if let Some(parent) = ndjson.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ctx = state.runs().get_or_create(&job.id, ndjson);
    let cancel = state.register_cancel(&job.id);

    let (tx, rx) = mpsc::channel::<RunEvent>();
    let runner = {
        let cancel = Arc::clone(&cancel);
        let cmd = runner_command(state.runner_bin(), std::env::vars_os().map(|(k, _)| k));
        let opts = opts.clone();
        std::thread::spawn(move || run_pipeline_subprocess_cmd(&opts, tx, cancel, cmd))
    };
    drive_job(state, &job, &ctx, &cancel, rx, runner);
}

/// Consume the events of a running job (a subject run or a group job), keep its progress row up to
/// date, and record its final phase when the runner has exited.
pub(crate) fn drive_job(
    state: &AppState,
    job: &JobRow,
    ctx: &Arc<crate::state::RunContext>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    rx: mpsc::Receiver<RunEvent>,
    runner: std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
) {
    let mut ok: u32 = 0;
    let mut failed: u32 = 0;
    let mut skipped: u32 = 0;
    let mut awaiting = false;
    let mut message: Option<String> = None;

    for ev in rx {
        let mut stage: Option<&str> = None;
        match &ev {
            RunEvent::StageStarted { stage: s } => stage = Some(s),
            RunEvent::StageCompleted { stage: s, .. } => {
                ok += 1;
                stage = Some(s);
            }
            RunEvent::StageSkipped { stage: s, .. } => {
                skipped += 1;
                stage = Some(s);
            }
            RunEvent::StageFailed {
                stage: s,
                message: m,
            } => {
                stage = Some(s);
                message = Some(m.clone());
                if !awaiting {
                    failed += 1;
                }
            }
            RunEvent::StageAwaitingQc { stage: s } => {
                awaiting = true;
                stage = Some(s);
                let _ = state.db().set_gated_stage(&job.id, Some(s));
            }
            RunEvent::PipelineCompleted {
                stages_ok,
                stages_failed,
                stages_skipped,
            } => {
                ok = *stages_ok;
                failed = if awaiting {
                    stages_failed.saturating_sub(1)
                } else {
                    *stages_failed
                };
                skipped = *stages_skipped;
            }
            RunEvent::LogLine { .. } => {}
        }
        if let Some(stage) = stage {
            let _ = state.db().update_progress(
                &job.id,
                (!stage.is_empty()).then_some(stage),
                ok,
                failed,
                skipped,
                message.as_deref(),
            );
        }
        ctx.record(&ev);
    }

    let cancelled = cancel.load(Ordering::SeqCst);
    let (exit_ok, exit_msg) = match runner.join() {
        Ok(Ok(st)) => {
            let ok = st.success();
            let msg = (!ok).then(|| {
                format!(
                    "pipeline runner exited with {}",
                    st.code()
                        .map_or_else(|| "a signal".to_owned(), |c| c.to_string())
                )
            });
            (ok, msg)
        }
        Ok(Err(e)) => (false, Some(format!("pipeline runner failed to start: {e}"))),
        Err(_) => (false, Some("pipeline runner thread panicked".to_owned())),
    };
    let phase = if cancelled {
        "cancelled"
    } else if awaiting {
        "awaiting_qc"
    } else if failed > 0 || !exit_ok {
        "failed"
    } else {
        "done"
    };
    if cancelled {
        message = Some(if state.is_shutdown() {
            "cancelled: server shutting down".to_owned()
        } else {
            "cancelled".to_owned()
        });
    } else if message.is_none() && !exit_ok && !awaiting {
        message = exit_msg;
    }
    let _ = state
        .db()
        .finish(&job.id, phase, ok, failed, skipped, message.as_deref());
    finalize_run(state, &job.id);
}

/// Read the persisted NDJSON of a run as `(sequence, event)` pairs; the sequence is the line index,
/// so an unparseable line leaves a gap instead of shifting the numbers.
pub fn read_ndjson_indexed(path: &std::path::Path) -> Vec<(u64, RunEvent)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| {
            serde_json::from_str::<RunEvent>(l)
                .ok()
                .map(|ev| (i as u64, ev))
        })
        .collect()
}

fn sanitized<T: serde::Serialize>(item: &T, sanitizer: &Sanitizer) -> Option<serde_json::Value> {
    let mut v = serde_json::to_value(item).ok()?;
    sanitizer.value(&mut v);
    Some(v)
}

fn sse_event(seq: u64, ev: &RunEvent, sanitizer: &Sanitizer) -> Event {
    sanitized(ev, sanitizer)
        .and_then(|v| Event::default().id(seq.to_string()).json_data(v).ok())
        .unwrap_or_else(|| Event::default().comment("unserialisable event"))
}

fn end_event(status: &dbsloc_api::RunStatus, sanitizer: &Sanitizer) -> Event {
    sanitized(status, sanitizer)
        .and_then(|v| Event::default().event(SSE_END_EVENT).json_data(v).ok())
        .unwrap_or_else(|| Event::default().event(SSE_END_EVENT).data("{}"))
}

fn after(last_event_id: Option<u64>, seq: u64) -> bool {
    last_event_id.is_none_or(|last| seq > last)
}

type SseStream = BoxStream<'static, Result<Event, Infallible>>;

/// Replay of a run whose bus is closed: the NDJSON lines, then the `end` event with its status.
fn terminal_stream(state: &AppState, job: &JobRow, last_event_id: Option<u64>) -> SseStream {
    let sanitizer = state.sanitizer(Some(&job.dataset));
    let end = Ok(end_event(&crate::dto::run_status(job), &sanitizer));
    let replay = read_ndjson_indexed(&state.run_ndjson(&job.dataset, &job.id))
        .into_iter()
        .filter(move |(seq, _)| after(last_event_id, *seq))
        .map(move |(seq, ev)| Ok(sse_event(seq, &ev, &sanitizer)));
    futures::stream::iter(replay.chain(std::iter::once(end))).boxed()
}

/// The SSE stream for a run. A queued or running run replays the NDJSON then follows the live bus
/// (every event carries its sequence as the SSE id; `last_event_id` skips what the client has);
/// any other run replays the NDJSON and ends with the `end` event. The stream also ends when the
/// server shuts down, and on a lagging subscriber (the browser reconnects with `Last-Event-ID`).
pub fn event_stream(
    state: &AppState,
    run_id: &str,
    last_event_id: Option<u64>,
) -> Result<SseStream, ServerErr> {
    let job = state
        .db()
        .get(run_id)?
        .ok_or_else(|| ServerErr::not_found(format!("no run {run_id}")))?;
    if !is_live_phase(&job.phase) {
        return Ok(terminal_stream(state, &job, last_event_id));
    }

    let ndjson = state.run_ndjson(&job.dataset, run_id);
    let ctx = state.runs().get_or_create(run_id, ndjson.clone());
    let subscription = ctx.subscribe();
    let current = state
        .db()
        .get(run_id)?
        .ok_or_else(|| ServerErr::not_found(format!("no run {run_id}")))?;
    let Some((rx, live_from)) = subscription.filter(|_| is_live_phase(&current.phase)) else {
        finalize_run(state, run_id);
        return Ok(terminal_stream(state, &current, last_event_id));
    };

    let sanitizer = Arc::new(state.sanitizer(Some(&job.dataset)));
    let replay_sanitizer = Arc::clone(&sanitizer);
    let replay = read_ndjson_indexed(&ndjson)
        .into_iter()
        .filter(move |(seq, _)| *seq < live_from && after(last_event_id, *seq))
        .map(move |(seq, ev)| Ok(sse_event(seq, &ev, &replay_sanitizer)));
    let live = BroadcastStream::new(rx)
        .take_while(|item| futures::future::ready(item.is_ok()))
        .filter_map(move |item| {
            futures::future::ready(match item {
                Ok(BusItem::Event(seq, ev)) if seq >= live_from && after(last_event_id, seq) => {
                    Some(Ok(sse_event(seq, &ev, &sanitizer)))
                }
                Ok(BusItem::End(status)) => Some(Ok(end_event(&status, &sanitizer))),
                _ => None,
            })
        });
    let mut shutdown = state.shutdown_receiver();
    let stop = async move {
        let _ = shutdown.wait_for(|down| *down).await;
    };
    Ok(futures::stream::iter(replay)
        .chain(live)
        .take_until(stop)
        .boxed())
}

/// Run status as a DTO, looked up in the database.
pub fn status(state: &AppState, run_id: &str) -> Result<dbsloc_api::RunStatus, ServerErr> {
    let job = state
        .db()
        .get(run_id)?
        .ok_or_else(|| ServerErr::not_found(format!("no run {run_id}")))?;
    Ok(crate::dto::run_status(&job))
}

/// The active run of a subject (`queued`, `running` or `awaiting_qc`), if any.
pub fn active_status(
    state: &AppState,
    dataset: &str,
    subject: &str,
) -> Result<Option<dbsloc_api::RunStatus>, ServerErr> {
    let sid = canonical_subject(subject)?;
    Ok(state
        .db()
        .active_for_subject(dataset, &sid)?
        .map(|job| crate::dto::run_status(&job)))
}

/// Cancel a run. A `queued` or `awaiting_qc` job becomes `cancelled` in one conditional update; a
/// `running` job gets its cancel flag set (a request that lands before the worker registers its
/// flag is kept) and the worker records the `cancelled` phase when the runner has stopped.
pub fn cancel(state: &AppState, run_id: &str) -> Result<(), ServerErr> {
    for _ in 0..3 {
        let job = state
            .db()
            .get(run_id)?
            .ok_or_else(|| ServerErr::not_found(format!("no run {run_id}")))?;
        match job.phase.as_str() {
            "queued" => {
                if state.db().transition(
                    run_id,
                    "queued",
                    "cancelled",
                    Some("cancelled before start"),
                )? {
                    finalize_run(state, run_id);
                    return Ok(());
                }
            }
            "awaiting_qc" => {
                if state.db().transition(
                    run_id,
                    "awaiting_qc",
                    "cancelled",
                    Some("cancelled while awaiting QC"),
                )? {
                    finalize_run(state, run_id);
                    return Ok(());
                }
            }
            "running" => {
                state.request_cancel(run_id);
                let still_active = state
                    .db()
                    .get(run_id)?
                    .is_some_and(|j| matches!(j.phase.as_str(), "running" | "queued"));
                if !still_active {
                    state.clear_cancel(run_id);
                }
                return Ok(());
            }
            other => {
                return Err(ServerErr::conflict(
                    E_SERVER,
                    format!("run {run_id} is {other} and cannot be cancelled"),
                ));
            }
        }
    }
    Err(ServerErr::conflict(
        E_SERVER,
        format!("run {run_id} changed state while it was being cancelled; retry"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> (tempfile::TempDir, AppState) {
        let d = tempfile::tempdir().unwrap();
        let config = crate::config::Config::default_for_root(d.path());
        let state = AppState::new_idle(config).unwrap();
        (d, state)
    }

    fn job(id: &str, subject: &str) -> NewJob {
        NewJob {
            id: id.to_owned(),
            dataset: "demo".to_owned(),
            subject: subject.to_owned(),
            options: "{}".to_owned(),
            priority: 1,
            resume: false,
            gated_stage: None,
            client: None,
        }
    }

    #[test]
    fn remaining_stages_after_a_gate() {
        let stages = vec![
            Stage::Preprocess,
            Stage::Coregister,
            Stage::Normalize,
            Stage::Reconstruct,
        ];
        assert_eq!(
            remaining_stages(&stages, Some("normalize")),
            vec![Stage::Reconstruct]
        );
        assert_eq!(
            remaining_stages(&stages, Some("coreg")),
            vec![Stage::Normalize, Stage::Reconstruct]
        );
        assert_eq!(remaining_stages(&stages, None), stages);
        assert_eq!(remaining_stages(&stages, Some("vat")), stages);
        assert!(remaining_stages(&stages, Some("reconstruct")).is_empty());
        assert_eq!(
            remaining_stages(&stages, Some("norm")),
            vec![Stage::Reconstruct]
        );
        assert_eq!(
            remaining_stages(&stages, Some("coregister")),
            vec![Stage::Normalize, Stage::Reconstruct]
        );
    }

    #[test]
    fn gate_labels_unify_branches_and_stage_names() {
        assert!(branch_matches_gate("norm", "normalize"));
        assert!(branch_matches_gate("normalization", "normalize"));
        assert!(branch_matches_gate("coreg", "coregister"));
        assert!(branch_matches_gate("coreg", "coreg"));
        assert!(branch_matches_gate("brainshift", "brainshift"));
        assert!(!branch_matches_gate("coreg", "normalize"));
        assert!(!branch_matches_gate("norm", "brainshift"));
    }

    #[test]
    fn subject_ids_are_canonicalised() {
        for raw in ["01", " 01", "sub-01", " sub-01 "] {
            assert_eq!(canonical_subject(raw).unwrap(), "01", "{raw}");
        }
        for bad in ["", "sub-", "a/b", "..", "sub-a b"] {
            assert!(canonical_subject(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cancel_ends_a_queued_job_and_a_later_cancel_conflicts() {
        let (_d, state) = test_state();
        state.db().insert(&job("r", "01")).unwrap();
        cancel(&state, "r").unwrap();
        let row = state.db().get("r").unwrap().unwrap();
        assert_eq!(row.phase, "cancelled");
        assert_eq!(row.message.as_deref(), Some("cancelled before start"));
        assert!(cancel(&state, "r").is_err());
        assert!(cancel(&state, "missing").is_err());
    }

    #[test]
    fn cancel_ends_an_awaiting_qc_job_and_frees_the_subject() {
        let (_d, state) = test_state();
        state
            .db()
            .insert_as(&job("r", "01"), "awaiting_qc")
            .unwrap();
        cancel(&state, "r").unwrap();
        assert_eq!(state.db().get("r").unwrap().unwrap().phase, "cancelled");
        assert!(state
            .db()
            .active_for_subject("demo", "01")
            .unwrap()
            .is_none());
    }

    #[test]
    fn cancel_of_a_running_job_sets_the_flag_and_leaves_the_phase_to_the_worker() {
        let (_d, state) = test_state();
        state.db().insert(&job("r", "01")).unwrap();
        state.db().take_next_queued().unwrap();
        cancel(&state, "r").unwrap();
        assert_eq!(state.db().get("r").unwrap().unwrap().phase, "running");
        assert!(state.has_pending_cancel("r"));
        assert!(state.register_cancel("r").load(Ordering::SeqCst));
    }

    #[test]
    fn rejecting_an_awaiting_job_records_the_stage() {
        let (_d, state) = test_state();
        let mut j = job("r", "01");
        j.gated_stage = Some("norm".to_owned());
        state.db().insert_as(&j, "awaiting_qc").unwrap();
        let row = state.db().get("r").unwrap().unwrap();
        assert!(reject_awaiting(&state, &row).unwrap());
        let row = state.db().get("r").unwrap().unwrap();
        assert_eq!(row.phase, "rejected");
        assert_eq!(row.message.as_deref(), Some("qc rejected: normalize"));
        assert!(!reject_awaiting(&state, &row).unwrap());
    }

    #[test]
    fn two_resume_submissions_create_exactly_one_resume_job() {
        let (d, state) = test_state();
        let opts = RunOptions::new(d.path().join("datasets/demo"), "01");
        let mut j = job("orig", "01");
        j.options = serde_json::to_string(&opts).unwrap();
        j.gated_stage = Some("normalize".to_owned());
        state.db().insert_as(&j, "awaiting_qc").unwrap();
        let original = state.db().get("orig").unwrap().unwrap();
        let first = submit_resume(&state, "demo", "01", &original, "t").unwrap();
        let second = submit_resume(&state, "demo", "01", &original, "t").unwrap();
        let ResumeOutcome::Resumed(id) = first else {
            panic!("first approval must resume: {first:?}");
        };
        assert_eq!(second, ResumeOutcome::Lost);
        let orig = state.db().get("orig").unwrap().unwrap();
        assert_eq!(orig.phase, "resumed");
        assert_eq!(orig.resumed_by.as_deref(), Some(id.as_str()));
        let resume = state.db().get(&id).unwrap().unwrap();
        assert!(resume.resume);
        assert_eq!(resume.priority, 0);
        assert_eq!(resume.phase, "queued");
    }

    #[test]
    fn a_gate_that_was_the_last_stage_retires_to_done() {
        let (d, state) = test_state();
        let mut opts = RunOptions::new(d.path().join("datasets/demo"), "01");
        opts.stages = vec![Stage::Coregister, Stage::Normalize];
        let mut j = job("orig", "01");
        j.options = serde_json::to_string(&opts).unwrap();
        j.gated_stage = Some("normalize".to_owned());
        state.db().insert_as(&j, "awaiting_qc").unwrap();
        let original = state.db().get("orig").unwrap().unwrap();
        assert_eq!(
            submit_resume(&state, "demo", "01", &original, "t").unwrap(),
            ResumeOutcome::Completed
        );
        assert_eq!(state.db().get("orig").unwrap().unwrap().phase, "done");
        assert!(state
            .db()
            .active_for_subject("demo", "01")
            .unwrap()
            .is_none());
    }

    #[test]
    fn finalize_closes_the_bus_with_the_final_status_and_forgets_the_run() {
        let (_d, state) = test_state();
        state.db().insert(&job("r", "01")).unwrap();
        let ctx = state
            .runs()
            .get_or_create("r", state.run_ndjson("demo", "r"));
        let (mut rx, _) = ctx.subscribe().unwrap();
        state.db().take_next_queued().unwrap();
        state.db().finish("r", "done", 1, 0, 0, None).unwrap();
        finalize_run(&state, "r");
        assert!(state.runs().get("r").is_none());
        match rx.try_recv() {
            Ok(BusItem::End(status)) => {
                assert_eq!(status.phase, dbsloc_api::RunPhase::Done);
            }
            other => panic!("expected the end item, got {other:?}"),
        }
    }

    #[test]
    fn run_options_force_stop_on_error_and_validate_the_grid() {
        let (_d, state) = test_state();
        let req = RunRequest {
            stop_on_error: Some(false),
            ..RunRequest::default()
        };
        let opts = build_run_options(&state, "demo", "01", &req).unwrap();
        assert!(opts.stop_on_error);
        assert_eq!(opts.reg_backend, RegBackend::Wgpu);
        for bad in [f64::NAN, f64::INFINITY, 0.0, 0.29, 3.01, -1.0] {
            let req = RunRequest {
                norm_isotropic_mm: Some(bad),
                ..RunRequest::default()
            };
            let err = build_run_options(&state, "demo", "01", &req).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(err.code(), E_BAD_REQUEST);
        }
        for good in [0.3, 1.0, 3.0] {
            let req = RunRequest {
                norm_isotropic_mm: Some(good),
                ..RunRequest::default()
            };
            assert!(build_run_options(&state, "demo", "01", &req).is_ok());
        }
        let full = RunRequest {
            norm_full_grid: Some(true),
            ..RunRequest::default()
        };
        assert_eq!(
            build_run_options(&state, "demo", "01", &full)
                .unwrap()
                .norm_isotropic_mm,
            dbsloc_api::NormGrid::Full
        );
        assert_eq!(
            build_run_options(&state, "demo", "01", &RunRequest::default())
                .unwrap()
                .norm_isotropic_mm,
            dbsloc_api::NormGrid::Template
        );
    }

    #[test]
    fn run_options_validate_method_names() {
        let (_d, state) = test_state();
        let req = RunRequest {
            norm_method: Some("fireants".to_owned()),
            ..RunRequest::default()
        };
        let err = build_run_options(&state, "demo", "01", &req).unwrap_err();
        assert_eq!(err.code(), "E_NORM_METHOD");
        let req = RunRequest {
            recon_method: Some("magic".to_owned()),
            ..RunRequest::default()
        };
        let err = build_run_options(&state, "demo", "01", &req).unwrap_err();
        assert_eq!(err.code(), E_BAD_REQUEST);
        for m in RECON_METHODS {
            let req = RunRequest {
                recon_method: Some((*m).to_owned()),
                ..RunRequest::default()
            };
            assert!(build_run_options(&state, "demo", "01", &req).is_ok(), "{m}");
        }
        assert_eq!(
            RECON_METHODS.contains(&"pacer"),
            dbsloc_pipeline::PACER_AVAILABLE
        );
        let req = RunRequest {
            recon_method: Some("pacer".to_owned()),
            ..RunRequest::default()
        };
        let got = build_run_options(&state, "demo", "01", &req);
        if dbsloc_pipeline::PACER_AVAILABLE {
            assert!(got.is_ok());
        } else {
            let err = got.unwrap_err();
            assert_eq!(err.code(), E_PACER_UNAVAILABLE);
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn ndjson_sequence_numbers_are_line_indices() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("r.ndjson");
        std::fs::write(
            &p,
            "{\"type\":\"log_line\",\"line\":\"a\"}\nnot json\n{\"type\":\"log_line\",\"line\":\"b\"}\n",
        )
        .unwrap();
        let rows = read_ndjson_indexed(&p);
        assert_eq!(rows.iter().map(|(s, _)| *s).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn the_job_environment_keeps_tool_locations_and_drops_run_options() {
        for (name, _) in dbsloc_pipeline::options::RUN_OPTION_ENV {
            assert!(!job_env_kept(name), "{name}");
            if cfg!(windows) {
                let lower = name.to_ascii_lowercase();
                assert!(!job_env_kept(&lower), "{lower}");
            }
        }
        for name in [
            "DBSLOC_PACER_RECO_JSON",
            "DBSLOC_SCRF_ANTS",
            "DBSLOC_SKIP_EXT_BIN_PREREQ",
        ] {
            assert!(!job_env_kept(name), "{name}");
        }
        for name in JOB_ENV_KEEP {
            assert!(job_env_kept(name), "{name}");
        }
        for name in [
            "DBSLOC_TEMPLATE_T1",
            "DBSLOC_TEMPLATE_T2",
            "DBSLOC_DATA_DIR",
        ] {
            assert!(
                job_env_kept(name),
                "{name}: the in-process plan reads it too"
            );
        }
        for name in [
            "DBSLOC_GPU_ADAPTER",
            "DBSLOC_GPU_MEMORY_BUDGET_MB",
            "PATH",
            "ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS",
        ] {
            assert!(job_env_kept(name), "{name}");
        }
        let cmd = runner_command(
            "runner",
            [
                "PATH",
                "DBSLOC_NORMALIZE_BACKEND",
                "DBSLOC_ANTS_DIR",
                "DBSLOC_STOP_ON_ERROR",
            ],
        );
        let mut removed: Vec<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            ["DBSLOC_NORMALIZE_BACKEND", "DBSLOC_STOP_ON_ERROR"]
        );
    }

    #[test]
    fn new_ids_are_distinct_and_hex() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }
}
