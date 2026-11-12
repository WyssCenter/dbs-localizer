// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Application state: config, job database, live run registry, cancellation flags, shutdown.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dbsloc_api::RunStatus;
use dbsloc_pipeline::protocol::RunEvent;
use tokio::sync::{broadcast, watch};

use crate::config::Config;
use crate::db::Db;
use crate::sanitize::Sanitizer;
use crate::uploads::UploadCache;

/// One item on a run's live bus.
#[derive(Debug, Clone)]
pub enum BusItem {
    /// A pipeline event with its sequence number (its line index in the run's NDJSON).
    Event(u64, RunEvent),
    /// The final status; the bus is closed right after it.
    End(Box<RunStatus>),
}

struct Bus {
    tx: Option<broadcast::Sender<BusItem>>,
    next_seq: u64,
}

/// One live run: its broadcast bus and persisted NDJSON stream.
pub struct RunContext {
    pub id: String,
    pub ndjson: PathBuf,
    bus: Mutex<Bus>,
}

impl RunContext {
    pub fn new(id: String, ndjson: PathBuf) -> RunContext {
        let (tx, _rx) = broadcast::channel(4096);
        RunContext {
            id,
            ndjson,
            bus: Mutex::new(Bus {
                tx: Some(tx),
                next_seq: 0,
            }),
        }
    }

    fn bus(&self) -> std::sync::MutexGuard<'_, Bus> {
        self.bus.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append `ev` to the run's NDJSON and broadcast it with its sequence number; a no-op once the
    /// bus is closed.
    pub fn record(&self, ev: &RunEvent) {
        let mut bus = self.bus();
        let Some(tx) = bus.tx.clone() else {
            return;
        };
        let line = serde_json::to_string(ev).unwrap_or_default();
        if let Some(parent) = self.ndjson.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ndjson)
        {
            let _ = writeln!(f, "{line}");
        }
        let seq = bus.next_seq;
        bus.next_seq += 1;
        let _ = tx.send(BusItem::Event(seq, ev.clone()));
    }

    /// Subscribe to the live bus; returns the receiver and the sequence number of the first live
    /// event (the NDJSON lines before it are the replay). `None` once the bus is closed.
    pub fn subscribe(&self) -> Option<(broadcast::Receiver<BusItem>, u64)> {
        let bus = self.bus();
        let rx = bus.tx.as_ref()?.subscribe();
        Some((rx, bus.next_seq))
    }

    /// Broadcast the final status and close the bus; idempotent.
    pub fn close(&self, status: RunStatus) {
        let mut bus = self.bus();
        if let Some(tx) = bus.tx.take() {
            let _ = tx.send(BusItem::End(Box::new(status)));
        }
    }
}

/// Registry of the runs whose bus is open (queued, running). A context is removed when its bus is
/// closed, so memory does not grow with the number of runs.
#[derive(Default)]
pub struct RunRegistry {
    map: Mutex<HashMap<String, Arc<RunContext>>>,
}

impl RunRegistry {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<RunContext>>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, id: &str) -> Option<Arc<RunContext>> {
        self.map().get(id).cloned()
    }

    /// The context of `id`, created with the NDJSON path `ndjson` when absent.
    pub fn get_or_create(&self, id: &str, ndjson: PathBuf) -> Arc<RunContext> {
        let mut m = self.map();
        Arc::clone(
            m.entry(id.to_owned())
                .or_insert_with(|| Arc::new(RunContext::new(id.to_owned(), ndjson))),
        )
    }

    pub fn remove(&self, id: &str) {
        self.map().remove(id);
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
struct CancelTable {
    flags: HashMap<String, Arc<AtomicBool>>,
    pending: HashSet<String>,
}

pub struct AppStateInner {
    pub config: Config,
    pub db: Db,
    pub runs: RunRegistry,
    cancels: Mutex<CancelTable>,
    /// The `dbsloc_pipeline_runner` binary path, resolved once at start-up.
    pub runner_bin: String,
    shutdown_tx: watch::Sender<bool>,
    active_jobs: AtomicUsize,
    sanitizer: Sanitizer,
    subject_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    uploads: UploadCache,
    preview_busy: AtomicBool,
    transfer: Mutex<Option<Arc<crate::connectomes::Transfer>>>,
}

/// Cheap, cloneable application handle (axum state).
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

/// Decrements the active-job counter when a job ends, however it ends.
pub struct JobGuard {
    inner: Arc<AppStateInner>,
}

/// Releases the interactive preview slot when dropped.
pub struct PreviewGuard {
    inner: Arc<AppStateInner>,
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.inner.preview_busy.store(false, Ordering::SeqCst);
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.inner.active_jobs.fetch_sub(1, Ordering::SeqCst);
    }
}

impl AppState {
    /// Build the state for `config`, open the database, reset crash state and start the job
    /// workers. The workers end when [`AppState::shutdown`] is called or the last handle drops.
    pub fn new(config: Config) -> std::io::Result<AppState> {
        let state = AppState::new_idle(config)?;
        state.start_workers();
        Ok(state)
    }

    /// [`AppState::new`] without the job workers (unit tests of the queue logic).
    pub fn new_idle(config: Config) -> std::io::Result<AppState> {
        std::fs::create_dir_all(config.datasets_dir())?;
        std::fs::create_dir_all(config.server_dir())?;
        std::fs::create_dir_all(config.incoming_dir())?;
        crate::uploads::sweep_stale_uploads(&config.incoming_dir());
        let db = Db::open(&config.server_dir().join("jobs.db"))
            .map_err(|e| std::io::Error::other(format!("open jobs.db: {e}")))?;
        let _ = db.reset_on_startup();
        let runner_bin = config.pipeline_runner_bin();
        let (shutdown_tx, _) = watch::channel(false);
        let sanitizer = Sanitizer::for_config(&config);
        Ok(AppState {
            inner: Arc::new(AppStateInner {
                config,
                db,
                runs: RunRegistry::default(),
                cancels: Mutex::new(CancelTable::default()),
                runner_bin,
                shutdown_tx,
                active_jobs: AtomicUsize::new(0),
                sanitizer,
                subject_locks: Mutex::new(HashMap::new()),
                uploads: UploadCache::default(),
                preview_busy: AtomicBool::new(false),
                transfer: Mutex::new(None),
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn runs(&self) -> &RunRegistry {
        &self.inner.runs
    }

    pub fn sanitizer(&self, dataset: Option<&str>) -> Sanitizer {
        match dataset.filter(|d| crate::files::valid_dataset_name(d)) {
            Some(ds) => self
                .inner
                .sanitizer
                .with_root(&self.dataset_root(ds), &format!("<datasets>/{ds}")),
            None => self.inner.sanitizer.clone(),
        }
    }

    pub fn subject_lock(&self, dataset: &str, subject: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .inner
            .subject_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Arc::clone(map.entry(format!("{dataset}/{subject}")).or_default())
    }

    pub fn uploads(&self) -> &UploadCache {
        &self.inner.uploads
    }

    /// Path of the persisted event stream for `dataset` / `run_id`.
    pub fn run_ndjson(&self, dataset: &str, run_id: &str) -> PathBuf {
        self.dataset_root(dataset)
            .join("runs")
            .join(format!("{run_id}.ndjson"))
    }

    /// Root of a dataset.
    pub fn dataset_root(&self, dataset: &str) -> PathBuf {
        self.config().datasets_dir().join(dataset)
    }

    /// Path of the audit log.
    pub fn audit_log(&self) -> PathBuf {
        self.config().server_dir().join("audit.log")
    }

    /// The resolved `dbsloc_pipeline_runner` binary path (fixed at start-up).
    pub fn runner_bin(&self) -> &str {
        &self.inner.runner_bin
    }

    fn cancels(&self) -> std::sync::MutexGuard<'_, CancelTable> {
        self.inner.cancels.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register the cancel flag of a job a worker is starting. A cancel requested before the
    /// worker got here, or a server shutdown, is already applied to the returned flag.
    pub fn register_cancel(&self, run_id: &str) -> Arc<AtomicBool> {
        let mut table = self.cancels();
        let flag = Arc::new(AtomicBool::new(
            table.pending.remove(run_id) || self.is_shutdown(),
        ));
        table.flags.insert(run_id.to_owned(), Arc::clone(&flag));
        flag
    }

    /// Request cancellation: sets the live flag when the worker registered one, else records a
    /// pending request that [`AppState::register_cancel`] consumes. Both happen under one lock, so
    /// a request can never fall between a worker's registration and its pending check.
    pub fn request_cancel(&self, run_id: &str) {
        let mut table = self.cancels();
        match table.flags.get(run_id) {
            Some(flag) => flag.store(true, Ordering::SeqCst),
            None => {
                table.pending.insert(run_id.to_owned());
            }
        }
    }

    /// Forget the cancel state of a finished job.
    pub fn clear_cancel(&self, run_id: &str) {
        let mut table = self.cancels();
        table.flags.remove(run_id);
        table.pending.remove(run_id);
    }

    #[cfg(test)]
    pub fn has_pending_cancel(&self, run_id: &str) -> bool {
        self.cancels().pending.contains(run_id)
    }

    /// Stop the server's background work: SSE streams end, running jobs are cancelled gracefully
    /// and the workers claim nothing more.
    pub fn shutdown(&self) {
        self.inner.shutdown_tx.send_replace(true);
        for flag in self.cancels().flags.values() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(t) = self.transfer_slot().as_ref() {
            t.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_shutdown(&self) -> bool {
        *self.inner.shutdown_tx.borrow()
    }

    /// A receiver that observes the shutdown signal.
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.inner.shutdown_tx.subscribe()
    }

    /// Wait until no job is running, at most `limit`; returns whether the workers are idle.
    pub async fn wait_for_jobs(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while self.inner.active_jobs.load(Ordering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    /// Number of jobs a worker is running right now.
    pub fn jobs_running(&self) -> usize {
        self.inner.active_jobs.load(Ordering::SeqCst)
    }

    /// Claim the single interactive stimulation preview; `None` while another one runs.
    pub fn try_begin_preview(&self) -> Option<PreviewGuard> {
        (!self.inner.preview_busy.swap(true, Ordering::SeqCst)).then(|| PreviewGuard {
            inner: Arc::clone(&self.inner),
        })
    }

    /// The connectome transfer slot (at most one transfer per server).
    pub fn transfer_slot(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<Arc<crate::connectomes::Transfer>>> {
        self.inner
            .transfer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Count a job as active until the returned guard drops.
    pub fn job_guard(&self) -> JobGuard {
        self.inner.active_jobs.fetch_add(1, Ordering::SeqCst);
        JobGuard {
            inner: Arc::clone(&self.inner),
        }
    }

    fn start_workers(&self) {
        let slots = self.config().gpu_jobs.max(1);
        for _ in 0..slots {
            let weak: Weak<AppStateInner> = Arc::downgrade(&self.inner);
            std::thread::spawn(move || crate::runs::worker_loop(weak));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, AppState) {
        let d = tempfile::tempdir().unwrap();
        let config = Config::default_for_root(d.path());
        let state = AppState::new_idle(config).unwrap();
        (d, state)
    }

    #[test]
    fn a_cancel_before_registration_reaches_the_flag() {
        let (_d, state) = state();
        state.request_cancel("r1");
        assert!(state.has_pending_cancel("r1"));
        let flag = state.register_cancel("r1");
        assert!(flag.load(Ordering::SeqCst));
        assert!(!state.has_pending_cancel("r1"));
    }

    #[test]
    fn a_cancel_after_registration_sets_the_live_flag() {
        let (_d, state) = state();
        let flag = state.register_cancel("r1");
        assert!(!flag.load(Ordering::SeqCst));
        state.request_cancel("r1");
        assert!(flag.load(Ordering::SeqCst));
        assert!(!state.has_pending_cancel("r1"));
        state.clear_cancel("r1");
        state.request_cancel("r1");
        state.clear_cancel("r1");
        assert!(!state.has_pending_cancel("r1"));
    }

    #[test]
    fn shutdown_cancels_registered_and_later_jobs() {
        let (_d, state) = state();
        let live = state.register_cancel("a");
        state.shutdown();
        assert!(state.is_shutdown());
        assert!(live.load(Ordering::SeqCst));
        assert!(state.register_cancel("b").load(Ordering::SeqCst));
    }

    #[test]
    fn runner_bin_is_the_configured_value() {
        let d = tempfile::tempdir().unwrap();
        let mut config = Config::default_for_root(d.path());
        config.pipeline_runner_bin = Some(PathBuf::from("/opt/runner"));
        let state = AppState::new_idle(config).unwrap();
        assert_eq!(state.runner_bin(), "/opt/runner");
    }

    fn status(id: &str) -> RunStatus {
        RunStatus {
            id: id.to_owned(),
            subject: "01".to_owned(),
            phase: dbsloc_api::RunPhase::Done,
            stage: None,
            stages_ok: 0,
            stages_failed: 0,
            stages_skipped: 0,
            started_at: None,
            finished_at: None,
            elapsed_ms: None,
            message: None,
            gated_stage: None,
            resumed_by: None,
        }
    }

    #[test]
    fn a_closed_bus_delivers_the_end_and_refuses_new_subscribers() {
        let d = tempfile::tempdir().unwrap();
        let ctx = RunContext::new("r".to_owned(), d.path().join("r.ndjson"));
        let (mut rx, first) = ctx.subscribe().unwrap();
        assert_eq!(first, 0);
        ctx.record(&RunEvent::LogLine { line: "a".into() });
        ctx.close(status("r"));
        ctx.record(&RunEvent::LogLine {
            line: "late".into(),
        });
        assert!(matches!(rx.try_recv(), Ok(BusItem::Event(0, _))));
        assert!(matches!(rx.try_recv(), Ok(BusItem::End(_))));
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(ctx.subscribe().is_none());
        let text = std::fs::read_to_string(d.path().join("r.ndjson")).unwrap();
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn the_registry_creates_once_and_removes() {
        let reg = RunRegistry::default();
        let a = reg.get_or_create("r", PathBuf::from("x"));
        let b = reg.get_or_create("r", PathBuf::from("y"));
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(reg.len(), 1);
        reg.remove("r");
        assert!(reg.is_empty());
        assert!(reg.get("r").is_none());
    }
}
