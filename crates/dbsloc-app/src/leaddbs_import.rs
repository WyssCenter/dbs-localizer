// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use dbsloc_pipeline::backdrop_fetch::free_space_bytes;
use dbsloc_pipeline::connectome_catalog::{Access, ConnectomeClass};
use dbsloc_pipeline::leaddbs_data::{
    self, bundled_atlas_names, link_plan, scan, user_atlases_dir_opt, AtlasItem, ConnectomeItem,
    CopyEvent, CopyOutcome, DataKind, Destinations, Inventory, LeadDbsDataError, LinkPlan,
    ScanContext, Selection,
};
use eframe::egui;

use crate::app::DbslocAppState;
use crate::connectome_panel::{bytes_label, is_narrow};
use crate::connectome_roots::resolve_with_setting;
use crate::panels::{em, palette, shorten_path, FolderPick};

const GB: u64 = 1_000_000_000;

pub const WINDOW_TITLE: &str = "Import data from Lead-DBS";

pub const ASK_TEXT: &str = "Do you already use Lead-DBS on this computer? Wyss DBS Localizer can use its atlases and connectomes, so you do not have to download them again.";
pub const LATER_TEXT: &str =
    "You can do this later from Connectomes… → Import → From a Lead-DBS installation…";
pub const NON_COMMERCIAL_NOTE: &str =
    "The Lead-DBS atlases come with the request of their owners not to use them commercially.";
pub const LINK_TEXT: &str = "No extra disk space. They stay in the Lead-DBS folder; if you move or remove Lead-DBS they are no longer available here.";
pub const LICENCE_HINT: &str = "These are usable now. Open Connectomes…, select the connectome and tick the boxes under Licence and terms to record your acceptance.";
pub const SCAN_STOPPED_TEXT: &str = "The scan stopped unexpectedly.";
pub const COPY_STOPPED_TEXT: &str = "the copy stopped unexpectedly";

/// The steps of the window (`specs/17` §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Ask,
    Scanning,
    Found,
    Copying,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Link,
    Copy,
}

/// Which items are ticked in the copy mode, in the order of the inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ticks {
    pub atlases: Vec<bool>,
    pub connectomes: Vec<bool>,
}

/// Where a copy would write, and how much room is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub atlases_dir: Option<PathBuf>,
    pub connectomes_root: PathBuf,
    pub free_atlases: Option<u64>,
    pub free_connectomes: Option<u64>,
    pub same_volume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportGate {
    Enabled,
    Disabled(String),
}

/// What the Done step shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub lines: Vec<String>,
    pub licence: Vec<String>,
}

/// What the application has to do after a frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Events {
    pub closed: bool,
    pub open_connectomes: bool,
    pub refresh: bool,
}

/// The first-start question is asked when it never was and the normal start screen is shown.
pub fn first_start_request(
    state: &DbslocAppState,
    normal_start: bool,
    screenshot_mode: bool,
) -> bool {
    !state.leaddbs_import_offered && normal_start && !screenshot_mode
}

/// [`first_start_request`], recording at once that the question is being asked.
pub fn begin_first_start(
    state: &mut DbslocAppState,
    normal_start: bool,
    screenshot_mode: bool,
) -> bool {
    let ask = first_start_request(state, normal_start, screenshot_mode);
    if ask {
        state.leaddbs_import_offered = true;
    }
    ask
}

/// The lines under the folder row: the notes of the scan, in the warning colour when nothing was found.
pub fn note_lines(inv: &Inventory) -> Vec<(String, bool)> {
    let warn = inv.is_empty();
    inv.notes.iter().map(|n| (n.clone(), warn)).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEnd {
    Found,
    Cancelled,
    Failed(String),
    Vanished,
}

/// The step the window returns to after a scan ended, and the error line to show there.
pub fn scan_return(has_inventory: bool, end: &ScanEnd) -> (Step, Option<String>) {
    let back = if has_inventory {
        Step::Found
    } else {
        Step::Ask
    };
    match end {
        ScanEnd::Found => (Step::Found, None),
        ScanEnd::Cancelled => (back, None),
        ScanEnd::Failed(text) => (back, Some(text.clone())),
        ScanEnd::Vanished => (back, Some(SCAN_STOPPED_TEXT.to_owned())),
    }
}

/// The failure of a copy worker that disappeared, with the items the window saw complete as its partial outcome.
pub fn vanished_copy_error(
    inv: &Inventory,
    done: &[(DataKind, String)],
    atlases_dir: &Path,
    connectomes_root: &Path,
) -> LeadDbsDataError {
    let mut copied = Vec::new();
    for (kind, name) in done {
        let (path, bytes) = match kind {
            DataKind::Atlas => (
                atlases_dir.join(name),
                inv.atlases
                    .iter()
                    .find(|a| &a.name == name)
                    .map(|a| a.bytes),
            ),
            DataKind::Connectome(class) => (
                connectomes_root.join(class.dir_name()).join(name),
                inv.connectomes
                    .iter()
                    .find(|c| c.class == *class && &c.name == name)
                    .map(|c| c.bytes),
            ),
        };
        copied.push(leaddbs_data::CopiedItem {
            kind: *kind,
            name: name.clone(),
            path,
            bytes: bytes.unwrap_or(0),
            skipped_links: 0,
        });
    }
    let atlases_dir = copied
        .iter()
        .any(|c| c.kind == DataKind::Atlas)
        .then(|| atlases_dir.to_path_buf());
    LeadDbsDataError::new(leaddbs_data::LeadDbsCode::Io, COPY_STOPPED_TEXT).with_partial(
        CopyOutcome {
            copied,
            skipped: Vec::new(),
            atlases_dir,
            notes: Vec::new(),
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EscapeContext {
    pub busy: bool,
    pub popup_open: bool,
    pub window_focused: bool,
    pub other_focused: bool,
    pub pointer_over: bool,
}

/// Whether *Esc* closes the window: not while a job runs, not when a popup or another part of the application has the key.
pub fn escape_closes(c: &EscapeContext) -> bool {
    !c.busy && !c.popup_open && !c.other_focused && (c.window_focused || c.pointer_over)
}

pub fn atlas_note(a: &AtlasItem) -> &'static str {
    if a.bundled {
        "Already included"
    } else {
        ""
    }
}

pub fn connectome_note(c: &ConnectomeItem) -> String {
    let mut notes = Vec::new();
    if c.present {
        notes.push("Already there");
    }
    match c.access {
        Some(Access::Dua) => notes.push("Needs your data use agreement"),
        Some(Access::CohortTerms) => notes.push("Cohort data terms"),
        _ => {}
    }
    notes.join("; ")
}

pub fn needs_licence_record(c: &ConnectomeItem) -> bool {
    matches!(c.access, Some(Access::Dua | Access::CohortTerms))
}

pub fn default_ticks(inv: &Inventory) -> Ticks {
    let sel = leaddbs_data::default_selection(inv);
    Ticks {
        atlases: inv
            .atlases
            .iter()
            .map(|a| sel.atlases.contains(&a.name))
            .collect(),
        connectomes: inv
            .connectomes
            .iter()
            .map(|c| sel.connectomes.contains(&(c.class, c.name.clone())))
            .collect(),
    }
}

pub fn selection_from(inv: &Inventory, ticks: &Ticks) -> Selection {
    Selection {
        atlases: inv
            .atlases
            .iter()
            .zip(&ticks.atlases)
            .filter(|(_, t)| **t)
            .map(|(a, _)| a.name.clone())
            .collect(),
        connectomes: inv
            .connectomes
            .iter()
            .zip(&ticks.connectomes)
            .filter(|(_, t)| **t)
            .map(|(c, _)| (c.class, c.name.clone()))
            .collect(),
    }
}

/// Bytes of the ticked atlases and of the ticked connectomes.
pub fn needed_bytes(inv: &Inventory, ticks: &Ticks) -> (u64, u64) {
    let atlases = inv
        .atlases
        .iter()
        .zip(&ticks.atlases)
        .filter(|(_, t)| **t)
        .map(|(a, _)| a.bytes)
        .sum();
    let connectomes = inv
        .connectomes
        .iter()
        .zip(&ticks.connectomes)
        .filter(|(_, t)| **t)
        .map(|(c, _)| c.bytes)
        .sum();
    (atlases, connectomes)
}

fn gb_text(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GB as f64)
}

/// The room to show next to the needed size: the smaller of the known free values.
pub fn free_for_display(dest: &Destination) -> Option<u64> {
    match (dest.free_atlases, dest.free_connectomes) {
        (Some(a), Some(c)) => Some(a.min(c)),
        (a, c) => a.or(c),
    }
}

/// Whether the ticked items fit; the needs are summed when both destinations are on one volume.
pub fn space_shortfall(
    need_atlases: u64,
    need_connectomes: u64,
    dest: &Destination,
) -> Option<String> {
    let short = |need: u64, free: u64| -> Option<String> {
        (need > 0 && free < need.saturating_add(GB)).then(|| {
            format!(
                "There is not enough free disk space: {} needed, {} free. Untick some items or free some space.",
                gb_text(need.saturating_add(GB)),
                gb_text(free)
            )
        })
    };
    match (dest.free_atlases, dest.free_connectomes) {
        (a, c) if dest.same_volume && (a.is_some() || c.is_some()) => {
            let free = match (a, c) {
                (Some(a), Some(c)) => a.min(c),
                (Some(f), None) | (None, Some(f)) => f,
                (None, None) => return None,
            };
            short(need_atlases.saturating_add(need_connectomes), free)
        }
        (a, c) => a
            .and_then(|f| short(need_atlases, f))
            .or_else(|| c.and_then(|f| short(need_connectomes, f))),
    }
}

/// Text under the *Copy* choice.
pub fn copy_text(need: u64, dest: Option<&Destination>) -> String {
    let free = dest.and_then(free_for_display).map_or_else(
        || "free space unknown".to_owned(),
        |f| format!("{} free", bytes_label(f)),
    );
    format!(
        "Needs {} ({free}). They stay available when Lead-DBS is removed.",
        bytes_label(need)
    )
}

/// Whether *Import* is enabled for the current choice, and if not, why.
pub fn import_gate(
    mode: Mode,
    inv: &Inventory,
    ticks: &Ticks,
    dest: Option<&Destination>,
) -> ImportGate {
    match mode {
        Mode::Link => {
            let plan = link_plan(inv);
            if plan.atlases_dir.is_none() && plan.connectomes_dir.is_none() {
                ImportGate::Disabled("Nothing was found in this folder.".to_owned())
            } else {
                ImportGate::Enabled
            }
        }
        Mode::Copy => {
            let sel = selection_from(inv, ticks);
            if sel.atlases.is_empty() && sel.connectomes.is_empty() {
                return ImportGate::Disabled("Tick at least one item to copy.".to_owned());
            }
            let Some(dest) = dest else {
                return ImportGate::Disabled("The destination is not known yet.".to_owned());
            };
            let (need_a, need_c) = needed_bytes(inv, ticks);
            if need_a > 0 && dest.atlases_dir.is_none() {
                return ImportGate::Disabled(
                    "There is no per-user data folder for atlases; set DBSLOC_DATA_DIR.".to_owned(),
                );
            }
            match space_shortfall(need_a, need_c, dest) {
                Some(reason) => ImportGate::Disabled(reason),
                None => ImportGate::Enabled,
            }
        }
    }
}

pub fn apply_link(state: &mut DbslocAppState, plan: &LinkPlan) {
    if let Some(dir) = &plan.atlases_dir {
        state.stage.leaddbs_atlases_dir = Some(dir.clone());
    }
    if let Some(dir) = &plan.connectomes_dir {
        state.stage.leaddbs_connectomes_dir = Some(dir.clone());
    }
}

pub fn apply_copy_outcome(state: &mut DbslocAppState, outcome: &CopyOutcome) {
    if let Some(dir) = &outcome.atlases_dir {
        state.stage.leaddbs_atlases_dir = Some(dir.clone());
    }
}

/// Stores the atlases directory of a copy that stopped early and returns the (notice, error)
/// texts, which list what was copied before the stop.
pub fn apply_copy_failure(
    state: &mut DbslocAppState,
    inv: &Inventory,
    e: &LeadDbsDataError,
) -> (Option<String>, Option<String>) {
    let mut before = String::new();
    if let Some(partial) = &e.partial {
        apply_copy_outcome(state, partial);
        let summary = copy_summary(inv, partial);
        if !partial.copied.is_empty() {
            before = format!(
                "\nCopied before the stop and kept:\n{}",
                summary.lines.join("\n")
            );
        }
    }
    if e.is_cancelled() {
        (
            Some(format!(
                "The copy was cancelled. Items already copied stay.{before}"
            )),
            None,
        )
    } else {
        (
            None,
            Some(format!(
                "{e}\nItems already copied stay; the Lead-DBS folder was not changed.{before}"
            )),
        )
    }
}

fn count_text(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The connectomes of the inventory that need a licence record, with their notes.
pub fn licence_list(inv: &Inventory, only: Option<&[(ConnectomeClass, String)]>) -> Vec<String> {
    inv.connectomes
        .iter()
        .filter(|c| needs_licence_record(c))
        .filter(|c| {
            only.is_none_or(|items| items.iter().any(|(k, n)| *k == c.class && *n == c.name))
        })
        .map(|c| format!("{}: {}", c.name, connectome_note(c)))
        .collect()
}

pub fn link_summary(inv: &Inventory, plan: &LinkPlan) -> Summary {
    let mut lines = Vec::new();
    if let Some(dir) = &plan.atlases_dir {
        lines.push(format!(
            "{} ({}) are used from {}.",
            count_text(inv.atlases.len(), "atlas", "atlases"),
            bytes_label(inv.atlas_bytes()),
            dir.display()
        ));
    }
    if let Some(dir) = &plan.connectomes_dir {
        lines.push(format!(
            "{} ({}) are used from {}.",
            count_text(inv.connectomes.len(), "connectome", "connectomes"),
            bytes_label(inv.connectome_bytes()),
            dir.display()
        ));
    }
    lines.push(
        "Nothing was copied. If you move or remove Lead-DBS they are no longer available here."
            .to_owned(),
    );
    Summary {
        lines,
        licence: licence_list(inv, None),
    }
}

pub fn copy_summary(inv: &Inventory, outcome: &CopyOutcome) -> Summary {
    let atlases: Vec<_> = outcome
        .copied
        .iter()
        .filter(|i| i.kind == DataKind::Atlas)
        .collect();
    let connectomes: Vec<_> = outcome
        .copied
        .iter()
        .filter(|i| i.kind != DataKind::Atlas)
        .collect();
    let mut lines = Vec::new();
    if !atlases.is_empty() {
        let bytes: u64 = atlases.iter().map(|i| i.bytes).sum();
        let at = outcome
            .atlases_dir
            .as_ref()
            .map_or_else(String::new, |d| format!(" into {}", d.display()));
        lines.push(format!(
            "Copied {} ({}){at}.",
            count_text(atlases.len(), "atlas", "atlases"),
            bytes_label(bytes)
        ));
    }
    if !connectomes.is_empty() {
        let bytes: u64 = connectomes.iter().map(|i| i.bytes).sum();
        lines.push(format!(
            "Copied {} ({}) into the Connectomes directory.",
            count_text(connectomes.len(), "connectome", "connectomes"),
            bytes_label(bytes)
        ));
    }
    if outcome.copied.is_empty() {
        lines.push("Nothing was copied.".to_owned());
    }
    if !outcome.skipped.is_empty() {
        let names: Vec<&str> = outcome.skipped.iter().map(|i| i.name.as_str()).collect();
        lines.push(format!("Already there, not copied: {}.", names.join(", ")));
    }
    for i in &outcome.copied {
        if let Some(text) = leaddbs_data::linked_entries_text(i.skipped_links) {
            lines.push(format!("{}: {text}.", i.name));
        }
    }
    for note in &outcome.notes {
        lines.push(format!("{note}."));
    }
    let copied: Vec<(ConnectomeClass, String)> = connectomes
        .iter()
        .filter_map(|i| match i.kind {
            DataKind::Connectome(class) => Some((class, i.name.clone())),
            DataKind::Atlas => None,
        })
        .collect();
    Summary {
        lines,
        licence: licence_list(inv, Some(&copied)),
    }
}

enum ScanMsg {
    Progress(usize),
    Done(Box<Result<(Inventory, Destination), LeadDbsDataError>>),
}

enum CopyMsg {
    Event(CopyEvent),
    Done(Box<Result<CopyOutcome, LeadDbsDataError>>),
}

struct ScanJob {
    rx: mpsc::Receiver<ScanMsg>,
    cancel: Arc<AtomicBool>,
    sized: usize,
}

impl Drop for ScanJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

struct CopyJob {
    rx: mpsc::Receiver<CopyMsg>,
    cancel: Arc<AtomicBool>,
    item: Option<(usize, usize, String)>,
    copied: u64,
    total: u64,
    done: Vec<(DataKind, String)>,
}

impl Drop for CopyJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

enum Act {
    Choose,
    Skip,
    CancelScan,
    CancelCopy,
    Import,
    OpenConnectomes,
}

/// The window *Import data from Lead-DBS*.
pub struct LeadDbsImport {
    open: bool,
    first_start: bool,
    step: Step,
    mode: Mode,
    scan_source: Option<PathBuf>,
    inventory: Option<Inventory>,
    ticks: Ticks,
    dest: Option<Destination>,
    pick: FolderPick,
    scan: Option<ScanJob>,
    copy: Option<CopyJob>,
    error: Option<String>,
    notice: Option<String>,
    summary: Option<Summary>,
    preload: Option<PathBuf>,
}

impl Default for LeadDbsImport {
    fn default() -> Self {
        LeadDbsImport {
            open: false,
            first_start: false,
            step: Step::Ask,
            mode: Mode::Link,
            scan_source: None,
            inventory: None,
            ticks: Ticks::default(),
            dest: None,
            pick: FolderPick::default(),
            scan: None,
            copy: None,
            error: None,
            notice: None,
            summary: None,
            preload: None,
        }
    }
}

impl LeadDbsImport {
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub fn step(&self) -> Step {
        self.step
    }

    pub fn is_busy(&self) -> bool {
        self.scan.is_some() || self.copy.is_some()
    }

    /// Whether nothing is pending, for screenshots.
    pub fn is_settled(&self) -> bool {
        self.preload.is_none() && self.scan.is_none() && !self.pick.is_open()
    }

    /// Opens the window at the first step; `first_start` words the skip button.
    pub fn open(&mut self, first_start: bool) {
        if self.open {
            return;
        }
        *self = LeadDbsImport {
            open: true,
            first_start,
            ..LeadDbsImport::default()
        };
    }

    /// Opens the window and scans `dir` at once (screenshots); `copy` preselects the copy choice.
    pub fn open_with_folder(&mut self, dir: PathBuf, copy: bool) {
        self.open(false);
        self.preload = Some(dir);
        if copy {
            self.mode = Mode::Copy;
        }
    }

    fn close(&mut self, state: &mut DbslocAppState, ev: &mut Events) {
        self.open = false;
        self.scan = None;
        self.copy = None;
        state.leaddbs_import_offered = true;
        ev.closed = true;
    }

    fn start_scan(&mut self, ctx: &egui::Context, state: &DbslocAppState, dir: PathBuf) {
        let roots = match resolve_with_setting(
            state.stage.connectomes_dir.as_deref(),
            state.stage.leaddbs_connectomes_dir.as_deref(),
        ) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(e.message);
                return;
            }
        };
        self.error = None;
        self.notice = None;
        self.scan_source = Some(dir.clone());
        self.step = Step::Scanning;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let resources =
                dbsloc_pipeline::bundled_bin::find_resources_root().map(|r| r.join("resources"));
            let primary = roots.primary.clone();
            let context = ScanContext {
                bundled_atlases: bundled_atlas_names(resources.as_deref()),
                roots,
            };
            let progress_tx = tx.clone();
            let progress_ctx = ctx.clone();
            let result = scan(&dir, &context, worker_cancel.as_ref(), &mut |n| {
                let _ = progress_tx.send(ScanMsg::Progress(n));
                progress_ctx.request_repaint();
            })
            .map(|inv| {
                let atlases_dir = user_atlases_dir_opt();
                let dest = Destination {
                    free_atlases: atlases_dir.as_deref().and_then(free_space_bytes),
                    free_connectomes: free_space_bytes(&primary),
                    same_volume: atlases_dir
                        .as_deref()
                        .is_some_and(|a| leaddbs_data::same_volume(a, &primary)),
                    atlases_dir,
                    connectomes_root: primary,
                };
                (inv, dest)
            });
            let _ = tx.send(ScanMsg::Done(Box::new(result)));
            ctx.request_repaint();
        });
        self.scan = Some(ScanJob {
            rx,
            cancel,
            sized: 0,
        });
    }

    fn start_copy(&mut self, ctx: &egui::Context) {
        let (Some(inv), Some(dest)) = (self.inventory.clone(), self.dest.clone()) else {
            return;
        };
        let Some(atlases_dir) = dest.atlases_dir.clone().or_else(user_atlases_dir_opt) else {
            self.error = Some("There is no per-user data folder; set DBSLOC_DATA_DIR.".to_owned());
            return;
        };
        let selection = selection_from(&inv, &self.ticks);
        let destinations = Destinations {
            atlases_dir,
            connectomes_root: dest.connectomes_root,
            free_space_override: None,
        };
        self.error = None;
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let event_tx = tx.clone();
            let event_ctx = ctx.clone();
            let result = leaddbs_data::copy(
                &inv,
                &selection,
                &destinations,
                worker_cancel.as_ref(),
                &mut |e| {
                    let _ = event_tx.send(CopyMsg::Event(e));
                    event_ctx.request_repaint();
                },
            );
            let _ = tx.send(CopyMsg::Done(Box::new(result)));
            ctx.request_repaint();
        });
        self.copy = Some(CopyJob {
            rx,
            cancel,
            item: None,
            copied: 0,
            total: 0,
            done: Vec::new(),
        });
        self.step = Step::Copying;
    }

    fn poll_scan(&mut self) {
        let mut finished = None;
        if let Some(job) = self.scan.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(ScanMsg::Progress(n)) => job.sized = n,
                    Ok(ScanMsg::Done(r)) => {
                        finished = Some(Some(*r));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = Some(None);
                        break;
                    }
                }
            }
        }
        let Some(result) = finished else { return };
        self.scan = None;
        self.scan_source = None;
        let end = match result {
            Some(Ok((inv, dest))) => {
                self.ticks = default_ticks(&inv);
                self.inventory = Some(inv);
                self.dest = Some(dest);
                ScanEnd::Found
            }
            Some(Err(e)) if e.is_cancelled() => ScanEnd::Cancelled,
            Some(Err(e)) => ScanEnd::Failed(e.to_string()),
            None => ScanEnd::Vanished,
        };
        let (step, error) = scan_return(self.inventory.is_some(), &end);
        self.step = step;
        if error.is_some() {
            self.error = error;
        }
    }

    fn poll_copy(&mut self, state: &mut DbslocAppState, ev: &mut Events) {
        let mut finished = None;
        if let Some(job) = self.copy.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(CopyMsg::Event(e)) => match e {
                        CopyEvent::ItemStarted {
                            name, index, count, ..
                        } => {
                            job.item = Some((index, count, name));
                            job.copied = 0;
                            job.total = 0;
                        }
                        CopyEvent::Bytes { copied, total } => {
                            job.copied = copied;
                            job.total = total;
                        }
                        CopyEvent::ItemDone { kind, name } => job.done.push((kind, name)),
                        CopyEvent::ItemSkipped { .. } | CopyEvent::AllDone => {}
                    },
                    Ok(CopyMsg::Done(r)) => {
                        finished = Some(*r);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let inv = self.inventory.clone().unwrap_or_else(empty_inventory);
                        let (atlases_dir, root) = self.dest.as_ref().map_or_else(
                            || (user_atlases_dir_opt().unwrap_or_default(), PathBuf::new()),
                            |d| {
                                (
                                    d.atlases_dir
                                        .clone()
                                        .or_else(user_atlases_dir_opt)
                                        .unwrap_or_default(),
                                    d.connectomes_root.clone(),
                                )
                            },
                        );
                        finished = Some(Err(vanished_copy_error(
                            &inv,
                            &job.done,
                            &atlases_dir,
                            &root,
                        )));
                        break;
                    }
                }
            }
        }
        let Some(result) = finished else { return };
        if self.copy.take().is_none() {
            return;
        }
        ev.refresh = true;
        match result {
            Ok(outcome) => {
                apply_copy_outcome(state, &outcome);
                let inv = self.inventory.clone().unwrap_or_else(empty_inventory);
                self.summary = Some(copy_summary(&inv, &outcome));
                self.step = Step::Done;
            }
            Err(e) => {
                let inv = self.inventory.clone().unwrap_or_else(empty_inventory);
                let (notice, error) = apply_copy_failure(state, &inv, &e);
                self.notice = notice;
                self.error = error;
                self.step = Step::Found;
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context, state: &mut DbslocAppState, ev: &mut Events) {
        if let Some(dir) = self.pick.poll() {
            if self.open && !self.is_busy() {
                self.start_scan(ctx, state, dir);
            }
        }
        if self.open && self.step == Step::Ask && self.scan.is_none() {
            if let Some(dir) = self.preload.take() {
                self.start_scan(ctx, state, dir);
            }
        }
        self.poll_scan();
        self.poll_copy(state, ev);
    }

    fn handle(
        &mut self,
        ctx: &egui::Context,
        state: &mut DbslocAppState,
        act: Act,
        ev: &mut Events,
    ) {
        match act {
            Act::Choose => {
                let start = self.inventory.as_ref().map(|i| i.source.clone());
                self.pick.open(ctx, "Select the Lead-DBS folder", start);
            }
            Act::Skip => self.close(state, ev),
            Act::CancelScan => {
                if let Some(job) = &self.scan {
                    job.cancel.store(true, Ordering::SeqCst);
                }
            }
            Act::CancelCopy => {
                if let Some(job) = &self.copy {
                    job.cancel.store(true, Ordering::SeqCst);
                }
            }
            Act::OpenConnectomes => {
                ev.open_connectomes = true;
                self.close(state, ev);
            }
            Act::Import => match self.mode {
                Mode::Link => {
                    let Some(inv) = self.inventory.clone() else {
                        return;
                    };
                    let plan = link_plan(&inv);
                    apply_link(state, &plan);
                    self.summary = Some(link_summary(&inv, &plan));
                    self.step = Step::Done;
                    ev.refresh = true;
                }
                Mode::Copy => self.start_copy(ctx),
            },
        }
    }

    /// Draws the window when it is open and keeps the workers moving either way.
    pub fn show(&mut self, ctx: &egui::Context, state: &mut DbslocAppState) -> Events {
        let mut ev = Events::default();
        self.poll(ctx, state, &mut ev);
        if !self.open {
            return ev;
        }
        let screen = ctx.content_rect().size();
        let unit = ctx.global_style().spacing.interact_size.y;
        let narrow = is_narrow(screen.x, unit);
        let mut acts: Vec<Act> = Vec::new();
        let mut open = true;
        let mut window = egui::Window::new(WINDOW_TITLE)
            .id(egui::Id::new("leaddbs_import_window"))
            .collapsible(false)
            .resizable(true)
            .default_width((unit * 34.0).min(screen.x * 0.92))
            .max_size(screen * 0.95)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
        if !self.is_busy() {
            window = window.open(&mut open);
        }
        let shown = window.show(ctx, |ui| {
            self.contents(ui, narrow, screen.y, &mut acts);
        });
        if let Some(shown) = shown {
            let layer = shown.response.layer_id;
            let focused = ctx.memory(|m| m.focused());
            let in_window = focused
                .and_then(|id| ctx.read_response(id))
                .is_some_and(|r| r.layer_id == layer);
            let escape = EscapeContext {
                busy: self.is_busy(),
                popup_open: egui::Popup::is_any_open(ctx),
                window_focused: in_window,
                other_focused: focused.is_some() && !in_window,
                pointer_over: ctx
                    .pointer_hover_pos()
                    .is_some_and(|p| ctx.layer_id_at(p) == Some(layer)),
            };
            if escape_closes(&escape)
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                self.close(state, &mut ev);
                return ev;
            }
        }
        if !open {
            self.close(state, &mut ev);
            return ev;
        }
        for act in acts {
            self.handle(ctx, state, act, &mut ev);
        }
        ev
    }

    fn contents(&mut self, ui: &mut egui::Ui, narrow: bool, screen_h: f32, acts: &mut Vec<Act>) {
        let pal = palette(ui);
        if let Some(error) = &self.error {
            ui.add(egui::Label::new(egui::RichText::new(error).color(pal.err)).wrap());
            ui.add_space(em(ui) * 0.3);
        }
        if let Some(notice) = &self.notice {
            ui.add(egui::Label::new(egui::RichText::new(notice).color(pal.text_muted)).wrap());
            ui.add_space(em(ui) * 0.3);
        }
        match self.step {
            Step::Ask => self.ask_ui(ui, acts),
            Step::Scanning => self.scanning_ui(ui, acts),
            Step::Found => self.found_ui(ui, narrow, screen_h, acts),
            Step::Copying => self.copying_ui(ui, acts),
            Step::Done => self.done_ui(ui, acts),
        }
    }

    fn ask_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let pal = palette(ui);
        ui.add(egui::Label::new(ASK_TEXT).wrap());
        ui.add_space(em(ui) * 0.5);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.pick.is_open(),
                    egui::Button::new("Choose Lead-DBS folder…"),
                )
                .on_hover_text(
                    "Choose the Lead-DBS installation folder, the folder that contains connectomes, or a connectomes folder. Nothing is changed in it.",
                )
                .clicked()
            {
                acts.push(Act::Choose);
            }
            let (label, tip) = if self.first_start {
                ("Skip", "Do not import now; this question is not asked again")
            } else {
                ("Cancel", "Close this window without importing")
            };
            if ui.button(label).on_hover_text(tip).clicked() {
                acts.push(Act::Skip);
            }
        });
        ui.add_space(em(ui) * 0.5);
        ui.add(
            egui::Label::new(
                egui::RichText::new(LATER_TEXT)
                    .small()
                    .color(pal.text_muted),
            )
            .wrap(),
        );
    }

    fn scanning_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let sized = self.scan.as_ref().map_or(0, |j| j.sized);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(em(ui)));
            ui.label(format!(
                "Looking at the folder… {} sized so far.",
                count_text(sized, "item", "items")
            ));
        });
        if let Some(src) = &self.scan_source {
            ui.add(egui::Label::new(shorten_path(src, 70)).selectable(false))
                .on_hover_text(src.display().to_string());
        }
        ui.add_space(em(ui) * 0.3);
        if ui
            .button("Cancel")
            .on_hover_text("Stop looking at the folder")
            .clicked()
        {
            acts.push(Act::CancelScan);
        }
    }

    fn found_ui(&mut self, ui: &mut egui::Ui, narrow: bool, screen_h: f32, acts: &mut Vec<Act>) {
        let Some(inv) = self.inventory.take() else {
            self.step = Step::Ask;
            return;
        };
        let pal = palette(ui);
        let unit = em(ui);
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(shorten_path(&inv.source, 60)).selectable(false))
                .on_hover_text(inv.source.display().to_string());
            if ui
                .add_enabled(
                    !self.pick.is_open(),
                    egui::Button::new("Choose another folder…").small(),
                )
                .on_hover_text("Look at a different Lead-DBS folder")
                .clicked()
            {
                acts.push(Act::Choose);
            }
        });
        ui.add_space(unit * 0.3);
        for (note, warn) in note_lines(&inv) {
            let text = if warn {
                egui::RichText::new(note).color(pal.warn)
            } else {
                egui::RichText::new(note).small().color(pal.text_muted)
            };
            ui.add(egui::Label::new(text).wrap());
        }
        if !inv.is_empty() {
            let copy_mode = self.mode == Mode::Copy;
            let list_h = if narrow {
                (screen_h * 0.2).max(unit * 5.0)
            } else {
                (screen_h * 0.34).clamp(unit * 6.0, unit * 18.0)
            };
            if narrow {
                atlas_group(ui, &inv, &mut self.ticks.atlases, copy_mode, list_h);
                ui.add_space(unit * 0.4);
                connectome_group(ui, &inv, &mut self.ticks.connectomes, copy_mode, list_h);
            } else {
                let ticks = &mut self.ticks;
                ui.columns(2, |cols| {
                    atlas_group(&mut cols[0], &inv, &mut ticks.atlases, copy_mode, list_h);
                    connectome_group(
                        &mut cols[1],
                        &inv,
                        &mut ticks.connectomes,
                        copy_mode,
                        list_h,
                    );
                });
            }
            ui.add_space(unit * 0.5);
            ui.separator();
            let (need_a, need_c) = needed_bytes(&inv, &self.ticks);
            ui.radio_value(&mut self.mode, Mode::Link, "Use them where they are")
                .on_hover_text(
                    "Store the two folders as settings and read the data in place; nothing is copied",
                );
            ui.add(
                egui::Label::new(egui::RichText::new(LINK_TEXT).small().color(pal.text_muted))
                    .wrap(),
            );
            ui.add_space(unit * 0.2);
            ui.radio_value(
                &mut self.mode,
                Mode::Copy,
                "Copy them into Wyss DBS Localizer",
            )
            .on_hover_text("Copy the ticked items; the Lead-DBS folder is never changed");
            ui.add(
                egui::Label::new(
                    egui::RichText::new(copy_text(need_a + need_c, self.dest.as_ref()))
                        .small()
                        .color(pal.text_muted),
                )
                .wrap(),
            );
        }
        ui.add_space(unit * 0.5);
        let gate = if inv.is_empty() {
            ImportGate::Disabled("Nothing was found in this folder.".to_owned())
        } else {
            import_gate(self.mode, &inv, &self.ticks, self.dest.as_ref())
        };
        ui.horizontal_wrapped(|ui| {
            let (enabled, reason) = match &gate {
                ImportGate::Enabled => (true, String::new()),
                ImportGate::Disabled(r) => (false, r.clone()),
            };
            let tip = match self.mode {
                Mode::Link => "Use the Lead-DBS folders where they are",
                Mode::Copy => "Copy the ticked items now",
            };
            if ui
                .add_enabled(enabled, egui::Button::new("Import"))
                .on_hover_text(tip)
                .on_disabled_hover_text(reason)
                .clicked()
            {
                acts.push(Act::Import);
            }
            if ui
                .button("Close")
                .on_hover_text("Close this window without importing (Esc)")
                .clicked()
            {
                acts.push(Act::Skip);
            }
        });
        if let ImportGate::Disabled(reason) = &gate {
            if !inv.is_empty() {
                ui.add(
                    egui::Label::new(egui::RichText::new(reason).small().color(pal.warn)).wrap(),
                );
            }
        }
        self.inventory = Some(inv);
    }

    fn copying_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let (title, fraction, text) = match self.copy.as_ref() {
            Some(job) => {
                let title = match &job.item {
                    Some((index, count, name)) => {
                        format!("Copying {name} (item {} of {count})", index + 1)
                    }
                    None => "Starting the copy…".to_owned(),
                };
                let fraction = if job.total > 0 {
                    (job.copied as f32 / job.total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let text = format!(
                    "{} of {}",
                    bytes_label(job.copied),
                    bytes_label(job.total.max(job.copied))
                );
                (title, fraction, text)
            }
            None => ("Copy finished".to_owned(), 1.0, String::new()),
        };
        ui.add(egui::Label::new(title).wrap());
        ui.add(egui::ProgressBar::new(fraction).text(text));
        ui.add_space(em(ui) * 0.3);
        if ui
            .button("Cancel")
            .on_hover_text("Stop copying. Items already copied stay; the current one is removed.")
            .clicked()
        {
            acts.push(Act::CancelCopy);
        }
    }

    fn done_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let pal = palette(ui);
        let unit = em(ui);
        let summary = self.summary.clone().unwrap_or_default();
        ui.label(egui::RichText::new("Done").strong());
        for line in &summary.lines {
            ui.add(egui::Label::new(line).wrap());
        }
        if !summary.licence.is_empty() {
            ui.add_space(unit * 0.5);
            ui.label(
                egui::RichText::new("Need your agreement record")
                    .strong()
                    .color(pal.warn),
            );
            for line in &summary.licence {
                ui.add(egui::Label::new(line).wrap());
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(LICENCE_HINT)
                        .small()
                        .color(pal.text_muted),
                )
                .wrap(),
            );
        }
        ui.add_space(unit * 0.5);
        ui.horizontal_wrapped(|ui| {
            if !summary.licence.is_empty()
                && ui
                    .button("Open Connectomes…")
                    .on_hover_text("Open the Connectomes window to record your acceptance")
                    .clicked()
            {
                acts.push(Act::OpenConnectomes);
            }
            if ui
                .button("Close")
                .on_hover_text("Close this window (Esc)")
                .clicked()
            {
                acts.push(Act::Skip);
            }
        });
    }
}

fn empty_inventory() -> Inventory {
    Inventory {
        source: PathBuf::new(),
        atlases_root: None,
        connectome_root: None,
        atlases: Vec::new(),
        connectomes: Vec::new(),
        notes: Vec::new(),
    }
}

fn item_row(
    ui: &mut egui::Ui,
    tick: Option<&mut bool>,
    name: &str,
    detail: &str,
    note: &str,
    attention: bool,
    tip: &str,
) {
    let pal = palette(ui);
    ui.horizontal_top(|ui| {
        if let Some(t) = tick {
            ui.checkbox(t, "")
                .on_hover_text("Copy this item into Wyss DBS Localizer");
        }
        ui.vertical(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(name).strong()).wrap());
            ui.add(
                egui::Label::new(egui::RichText::new(detail).small().color(pal.text_muted)).wrap(),
            );
            if !note.is_empty() {
                let color = if attention { pal.warn } else { pal.text_muted };
                ui.add(egui::Label::new(egui::RichText::new(note).small().color(color)).wrap());
            }
        })
        .response
        .on_hover_text(tip);
    });
    ui.add_space(em(ui) * 0.2);
}

fn atlas_group(
    ui: &mut egui::Ui,
    inv: &Inventory,
    ticks: &mut [bool],
    copy_mode: bool,
    height: f32,
) {
    let pal = palette(ui);
    ui.label(
        egui::RichText::new(format!(
            "Atlases ({}, {})",
            inv.atlases.len(),
            bytes_label(inv.atlas_bytes())
        ))
        .strong(),
    );
    if inv.atlases.is_empty() {
        ui.label(egui::RichText::new("None found.").color(pal.text_muted));
        return;
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(NON_COMMERCIAL_NOTE)
                .small()
                .color(pal.text_muted),
        )
        .wrap(),
    );
    egui::ScrollArea::vertical()
        .id_salt("leaddbs_import_atlases")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, a) in inv.atlases.iter().enumerate() {
                let tick = if copy_mode { ticks.get_mut(i) } else { None };
                let tip = format!("{}\n{} files", a.path.display(), a.files);
                item_row(
                    ui,
                    tick,
                    &a.name,
                    &bytes_label(a.bytes),
                    atlas_note(a),
                    false,
                    &tip,
                );
            }
        });
}

fn connectome_group(
    ui: &mut egui::Ui,
    inv: &Inventory,
    ticks: &mut [bool],
    copy_mode: bool,
    height: f32,
) {
    let pal = palette(ui);
    ui.label(
        egui::RichText::new(format!(
            "Connectomes ({}, {})",
            inv.connectomes.len(),
            bytes_label(inv.connectome_bytes())
        ))
        .strong(),
    );
    if inv.connectomes.is_empty() {
        ui.label(egui::RichText::new("None found.").color(pal.text_muted));
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("leaddbs_import_connectomes")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, c) in inv.connectomes.iter().enumerate() {
                let tick = if copy_mode { ticks.get_mut(i) } else { None };
                let tip = format!("{}\n{} files", c.path.display(), c.files);
                let detail = format!("{} · {}", c.class.kind_label(), bytes_label(c.bytes));
                item_row(
                    ui,
                    tick,
                    &c.name,
                    &detail,
                    &connectome_note(c),
                    needs_licence_record(c),
                    &tip,
                );
            }
        });
}

#[cfg(test)]
#[path = "leaddbs_import_tests.rs"]
mod tests;
