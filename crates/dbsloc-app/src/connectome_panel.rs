// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use dbsloc_pipeline::backdrop_fetch::free_space_bytes;
use dbsloc_pipeline::connectome_catalog::{self, Access, ConnectomeClass, ConnectomeEntry, Step};
use dbsloc_pipeline::connectome_fetch::{
    self, Acceptance, ConnectomeError, ConnectomeEvent, ConnectomeRoots, ConnectomeState,
    ConnectomeStatus, ErrorCode, FetchSettings, InstalledConnectome, RootKind,
};
use eframe::egui;

use crate::app::DbslocAppState;
use crate::connectome_roots::{current_external_source, resolve_with_setting};
use crate::panels::{
    callout, em, info_icon, open_folder_button, palette, shorten_path, FolderPick,
};

const GB: u64 = 1_000_000_000;

const LEADDBS_ROOT_INFO: &str = "A Lead-DBS connectomes folder that is read where it is. \
     Nothing is ever written there. The folder chosen here takes precedence over the \
     environment variable DBSLOC_LEADDBS_CONNECTOMES_DIR.";

pub fn leaddbs_folder_text(source: &crate::connectome_roots::ExternalSource) -> String {
    use crate::connectome_roots::ExternalSource;
    match source {
        ExternalSource::Setting(p) => p.display().to_string(),
        ExternalSource::Environment(p) => {
            format!("{} (from the environment variable)", p.display())
        }
        ExternalSource::NotSet => "not set".to_owned(),
    }
}

const DIRECTORY_INFO: &str = "Downloads and extracted connectomes are stored here. Connectomes \
     are large, so choose a drive with enough free space. Folders you register stay where they \
     are.";

const BUSY_REASON: &str = "Another download is running. Wait for it to finish or cancel it.";
const OFFLINE_REASON: &str =
    "The download server could not be reached at the last attempt. Use Check again to retry.";
const DISK_REASON: &str = "There is not enough free disk space on the drive of the Connectomes \
     directory. Free some space or change the directory.";

/// Which boxes the user ticked for one connectome.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ticks {
    pub accept_terms: bool,
    pub attest: bool,
}

/// What the download button of a card does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadControl {
    Hidden,
    Enabled,
    Disabled(&'static str),
}

/// Everything the card's button state depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardContext {
    pub access: Access,
    pub installed: bool,
    pub ticks: Ticks,
    pub download_busy: bool,
    pub import_busy: bool,
    pub offline: bool,
    pub disk_short: bool,
}

/// Which parts and buttons a card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardControls {
    pub show_instructions: bool,
    pub show_attestation: bool,
    pub download: DownloadControl,
    pub choose_folder: bool,
    pub import_archive: bool,
}

/// Whether the ticks required for this access class are set.
pub fn terms_complete(access: Access, ticks: Ticks) -> bool {
    ticks.accept_terms && (access != Access::Dua || ticks.attest)
}

/// The button state machine of one card (`specs/15` §4.2).
pub fn card_controls(c: &CardContext) -> CardControls {
    let download = if c.installed || c.access == Access::Dua || !terms_complete(c.access, c.ticks) {
        DownloadControl::Hidden
    } else if c.download_busy {
        DownloadControl::Disabled(BUSY_REASON)
    } else if c.offline {
        DownloadControl::Disabled(OFFLINE_REASON)
    } else if c.disk_short {
        DownloadControl::Disabled(DISK_REASON)
    } else {
        DownloadControl::Enabled
    };
    CardControls {
        show_instructions: true,
        show_attestation: c.access == Access::Dua,
        download,
        choose_folder: !c.import_busy,
        import_archive: !c.import_busy,
    }
}

/// Whether a licence record of the current terms exists for a connectome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    Missing,
    Current,
    Stale,
}

/// What the *Record acceptance* button of an installed connectome does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordControl {
    Hidden,
    Enabled,
    Disabled(&'static str),
}

const RECORD_TERMS_REASON: &str =
    "Tick the box to accept the terms first. The acceptance is recorded only when you do.";
const RECORD_ATTEST_REASON: &str =
    "Tick both boxes first: the terms and the statement about your own data use agreement.";

/// Whether an installed connectome still needs the user's acceptance recorded (`specs/15` §4.2).
pub fn acceptance_record_needed(access: Access, installed: bool, record: RecordState) -> bool {
    installed && access != Access::Open && record != RecordState::Current
}

/// The *Record acceptance* button: hidden when nothing is needed, disabled with a reason until
/// the ticks required for the access class are set.
pub fn record_control(
    access: Access,
    installed: bool,
    record: RecordState,
    ticks: Ticks,
) -> RecordControl {
    if !acceptance_record_needed(access, installed, record) {
        RecordControl::Hidden
    } else if !ticks.accept_terms {
        RecordControl::Disabled(RECORD_TERMS_REASON)
    } else if !terms_complete(access, ticks) {
        RecordControl::Disabled(RECORD_ATTEST_REASON)
    } else {
        RecordControl::Enabled
    }
}

/// Writes the licence record of an installed connectome and returns the confirmation text.
pub fn record_acceptance(
    primary: &Path,
    entry: &ConnectomeEntry,
    ticks: Ticks,
) -> Result<String, ConnectomeError> {
    let acceptance = Acceptance {
        accept_terms: ticks.accept_terms,
        attest_data_use_agreement: ticks.attest,
        via: "gui",
    };
    connectome_fetch::check_acceptance(entry, &acceptance)?;
    let attestation =
        (entry.access == Access::Dua && ticks.attest).then(|| entry.attestation_sentence());
    connectome_fetch::write_licence_record(primary, entry, acceptance.via, attestation.as_deref())?;
    Ok(format!(
        "Recorded your acceptance of the terms of {}.",
        entry.name
    ))
}

/// What is still missing before a download can start, for the hint under the ticks.
pub fn missing_acceptance_hint(access: Access, ticks: Ticks) -> Option<&'static str> {
    if access == Access::Dua {
        return Some(if terms_complete(access, ticks) {
            "Your acceptance is recorded with the dataset when you import it."
        } else {
            "Importing works without the boxes, but then no licence record is written. Tick both boxes to record your acceptance when you import."
        });
    }
    if !ticks.accept_terms {
        Some("Tick the box above to enable the download.")
    } else {
        None
    }
}

/// Text of the state line of a card; an import-only entry that is not installed says so.
pub fn card_state_text(
    access: Access,
    state: &ConnectomeState,
    origin: Option<RootKind>,
) -> String {
    match (access, state) {
        (Access::Dua, ConnectomeState::Available { .. }) => {
            "Import only: needs your own data access".to_owned()
        }
        _ => state_text(state, origin),
    }
}

/// Whether the "How to get this dataset" section starts open.
pub fn how_section_default_open(access: Access) -> bool {
    access == Access::Dua
}

/// One line of the numbered list: the number and the sentence.
pub fn step_line(index: usize, step: &Step) -> String {
    format!("{}. {}", index + 1, step.text)
}

/// How the access class is described in a row.
pub fn access_text(access: Access) -> &'static str {
    match access {
        Access::Open => "Open licence",
        Access::CohortTerms => "Cohort data terms",
        Access::Dua => "Needs your data use agreement",
    }
}

/// Size as shown in the lists (decimal units): gigabytes from one gigabyte, megabytes from one
/// megabyte, kilobytes from one kilobyte, else bytes.
pub fn bytes_label(bytes: u64) -> String {
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= 999_500 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} kB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

/// Remaining time in words.
pub fn eta_label(secs: f64) -> String {
    if secs < 60.0 {
        "less than a minute".to_owned()
    } else if secs < 3600.0 {
        format!("{:.0} min", (secs / 60.0).ceil())
    } else {
        let minutes = (secs / 60.0).round() as u64;
        format!("{} h {} min", minutes / 60, minutes % 60)
    }
}

/// The text on the progress bar: bytes, rate and time left.
pub fn progress_text(downloaded: u64, total: u64, rate: Option<f64>) -> String {
    let mut text = format!(
        "{} of {}",
        bytes_label(downloaded),
        bytes_label(total.max(downloaded))
    );
    if let Some(rate) = rate.filter(|r| *r > 0.0) {
        text.push_str(&format!(" · {:.1} MB/s", rate / 1_000_000.0));
        let remaining = total.saturating_sub(downloaded) as f64;
        text.push_str(&format!(" · about {} left", eta_label(remaining / rate)));
    }
    text
}

/// Bytes per second from the first sample of a transfer; `None` until two seconds have passed.
pub fn transfer_rate(first: Option<(f64, u64)>, now_secs: f64, downloaded: u64) -> Option<f64> {
    let (t0, b0) = first?;
    let elapsed = now_secs - t0;
    (elapsed >= 2.0 && downloaded > b0).then(|| (downloaded - b0) as f64 / elapsed)
}

/// An error code with its plain-language meaning and the technical detail.
pub fn plain_message(e: &ConnectomeError) -> String {
    let plain = match e.code {
        ErrorCode::Cancelled => {
            "The download was cancelled. The partial file is kept and the next download continues from it."
        }
        ErrorCode::Busy => {
            "Another connectome download is already running, here or in another window. Wait for it to finish, then try again."
        }
        ErrorCode::Host => {
            "The download server cannot be reached. Check the internet connection and try again; the partial download is kept."
        }
        ErrorCode::Terms => "The terms were not accepted. Tick the boxes first.",
        ErrorCode::Disk => {
            "There is not enough free disk space. Free some space or choose another Connectomes directory."
        }
        ErrorCode::Checksum => {
            "The downloaded file does not match the published checksum, so it was discarded. Download it again."
        }
        ErrorCode::Changed => {
            "The copy on the server changed since this version of Wyss DBS Localizer was released. Update the application."
        }
        ErrorCode::Size => "The server sent more data than expected. Try again later.",
        ErrorCode::Archive => {
            "The archive holds unsafe or unreadable entries and was not extracted."
        }
        ErrorCode::Format => "This is not a connectome folder this application can read.",
        ErrorCode::Missing => "The folder or file was not found.",
        ErrorCode::Io => "A file could not be read or written.",
        ErrorCode::Usage => "The request cannot be carried out as given.",
    };
    if e.code == ErrorCode::Cancelled {
        plain.to_owned()
    } else {
        format!("{}: {plain}\nDetails: {}", e.code.code(), e.message)
    }
}

/// Date part of an ISO 8601 time stamp.
pub fn date_of(stamp: &str) -> &str {
    stamp.get(..10).unwrap_or(stamp)
}

/// The state line of a row.
pub fn state_text(state: &ConnectomeState, origin: Option<RootKind>) -> String {
    match (state, origin) {
        (ConnectomeState::Installed(p), Some(RootKind::External)) => {
            format!("External Lead-DBS folder at {}", p.display())
        }
        (ConnectomeState::Installed(p), Some(RootKind::Registered)) => {
            format!("Registered folder at {}", p.display())
        }
        (ConnectomeState::Installed(p), _) => format!("Installed at {}", p.display()),
        (ConnectomeState::Partial { percent, .. }, _) => {
            format!("Partial download ({percent} %)")
        }
        (ConnectomeState::Available { .. }, _) => "Not installed".to_owned(),
    }
}

/// Short badge text of a row.
pub fn badge_text(state: &ConnectomeState, origin: Option<RootKind>) -> &'static str {
    match (state, origin) {
        (ConnectomeState::Installed(_), Some(RootKind::External)) => "External folder",
        (ConnectomeState::Installed(_), Some(RootKind::Registered)) => "Registered folder",
        (ConnectomeState::Installed(_), _) => "Installed",
        (ConnectomeState::Partial { .. }, _) => "Partial download",
        (ConnectomeState::Available { .. }, _) => "Not installed",
    }
}

/// Whether the free space cannot hold the rest of the download.
pub fn disk_short(free: Option<u64>, entry: &ConnectomeEntry, partial: u64) -> bool {
    let need = connectome_fetch::required_free_bytes(entry, GB).saturating_sub(partial);
    free.is_some_and(|f| f < need)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ListFilter {
    #[default]
    All,
    Installed,
    Structural,
    Functional,
}

impl ListFilter {
    pub const ALL: [ListFilter; 4] = [
        ListFilter::All,
        ListFilter::Installed,
        ListFilter::Structural,
        ListFilter::Functional,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ListFilter::All => "All",
            ListFilter::Installed => "Installed",
            ListFilter::Structural => "Structural",
            ListFilter::Functional => "Functional",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            ListFilter::All => "Show every connectome",
            ListFilter::Installed => "Show only the connectomes that are ready to use",
            ListFilter::Structural => "Show fibre-tract connectomes (diffusion imaging)",
            ListFilter::Functional => "Show functional connectomes (correlated activity)",
        }
    }
}

pub fn passes_filter(filter: ListFilter, installed: bool, class: ConnectomeClass) -> bool {
    match filter {
        ListFilter::All => true,
        ListFilter::Installed => installed,
        ListFilter::Structural => class.kind_label() == "Structural",
        ListFilter::Functional => class.kind_label() == "Functional",
    }
}

pub fn split_rows(
    items: &[(bool, ConnectomeClass)],
    filter: ListFilter,
) -> (Vec<usize>, Vec<usize>) {
    let mut installed = Vec::new();
    let mut available = Vec::new();
    for (i, (is_installed, class)) in items.iter().enumerate() {
        if !passes_filter(filter, *is_installed, *class) {
            continue;
        }
        if *is_installed {
            installed.push(i);
        } else {
            available.push(i);
        }
    }
    (installed, available)
}

pub fn other_visible(filter: ListFilter, present: bool, class: ConnectomeClass) -> bool {
    passes_filter(filter, present, class)
}

pub fn default_selection(installed: &[bool]) -> Option<usize> {
    installed
        .iter()
        .position(|i| *i)
        .or(if installed.is_empty() { None } else { Some(0) })
}

pub fn licence_open_by_default(installed: bool, terms_accepted: bool) -> bool {
    !installed && !terms_accepted
}

pub fn is_narrow(width: f32, unit: f32) -> bool {
    width < unit * 45.0
}

pub fn list_width(width: f32, unit: f32) -> f32 {
    (width * 0.4).max(unit * 18.5).min(width * 0.5)
}

pub fn row_subtitle(kind: &str, state: &ConnectomeState, total_bytes: u64) -> String {
    match state {
        ConnectomeState::Partial { percent, .. } => format!("{kind} · partial {percent} %"),
        _ => format!("{kind} · {}", bytes_label(total_bytes)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    Entry(&'static str),
    Other(PathBuf),
}

impl Row {
    fn is_installed(&self) -> bool {
        matches!(self.status.state, ConnectomeState::Installed(_))
    }
}

#[derive(Debug, Clone)]
struct Row {
    status: ConnectomeStatus,
    origin: Option<RootKind>,
    version: Option<String>,
}

#[derive(Debug, Clone)]
struct RecordInfo {
    accepted_at: String,
    stale: bool,
}

#[derive(Debug, Clone)]
struct Listing {
    roots: ConnectomeRoots,
    rows: Vec<Row>,
    others: Vec<InstalledConnectome>,
    records: HashMap<&'static str, RecordInfo>,
    free: Option<u64>,
}

fn build_listing(roots: ConnectomeRoots) -> Listing {
    let installed = connectome_fetch::discover_connectomes(&roots);
    let rows: Vec<Row> = connectome_fetch::list_catalogue(&roots, &installed)
        .into_iter()
        .map(|status| {
            let found = installed.iter().find(|c| {
                c.catalogue_id == Some(status.entry.id) && !c.missing && c.duplicate_of.is_none()
            });
            Row {
                origin: found.map(|c| c.root_kind),
                version: found.and_then(|c| c.version.clone()),
                status,
            }
        })
        .collect();
    let others = installed
        .into_iter()
        .filter(|c| c.catalogue_id.is_none() && c.duplicate_of.is_none())
        .collect();
    let records = connectome_catalog::catalogue()
        .iter()
        .filter_map(|e| {
            let rec = connectome_fetch::read_licence_record(&roots.primary, e.id)?;
            Some((
                e.id,
                RecordInfo {
                    accepted_at: rec["accepted_at"].as_str().unwrap_or_default().to_owned(),
                    stale: connectome_fetch::licence_record_is_stale(&rec, e),
                },
            ))
        })
        .collect();
    let free = free_space_bytes(&roots.primary);
    Listing {
        roots,
        rows,
        others,
        records,
        free,
    }
}

enum Msg {
    Event(ConnectomeEvent),
    Done(Result<String, ConnectomeError>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Downloading,
    Verifying,
    Extracting,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Fetch,
    Import,
}

struct Job {
    kind: JobKind,
    label: String,
    entry_id: Option<&'static str>,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    cancelled: bool,
    downloaded: u64,
    total: u64,
    file: Option<(usize, usize)>,
    phase: Phase,
    retry: Option<String>,
    started: Instant,
    first_sample: Option<(f64, u64)>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Job {
    fn rate(&self) -> Option<f64> {
        transfer_rate(
            self.first_sample,
            self.started.elapsed().as_secs_f64(),
            self.downloaded,
        )
    }
}

#[derive(Default)]
struct ImportPick {
    rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    archive: bool,
    target: Option<&'static str>,
}

impl ImportPick {
    fn is_open(&self) -> bool {
        self.rx.is_some()
    }

    fn open(&mut self, ctx: &egui::Context, archive: bool, target: Option<&'static str>) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.archive = archive;
        self.target = target;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let pick = if archive {
                rfd::FileDialog::new()
                    .set_title("Select the connectome archive")
                    .add_filter("Archive", &["zip", "tar", "gz", "tgz"])
                    .pick_file()
            } else {
                rfd::FileDialog::new()
                    .set_title("Select the connectome folder")
                    .pick_folder()
            };
            let _ = tx.send(pick);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) -> Option<(PathBuf, bool, Option<&'static str>)> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(pick) => {
                self.rx = None;
                pick.map(|p| (p, self.archive, self.target))
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }
}

enum Action {
    Download(&'static str),
    Import {
        archive: bool,
        target: Option<&'static str>,
    },
    OpenUrl(String),
    Forget(PathBuf),
    RecordAcceptance(&'static str),
    Cancel,
    CheckAgain,
    Refresh,
    ChangeDirectory,
    UseDefaultDirectory,
    ChangeLeadDbsFolder,
    ClearLeadDbsFolder,
    ImportLeadDbs,
    RevealDirectory,
    Reveal(PathBuf),
    SetCache(crate::connectome_cache_ui::CacheMode),
    ManageCache,
}

/// The *Connectomes* window: catalogue, access instructions, download and import.
#[derive(Default)]
pub struct ConnectomePanel {
    open: bool,
    listing: Option<Listing>,
    listing_rx: Option<mpsc::Receiver<Listing>>,
    refresh_wanted: bool,
    ticks: HashMap<&'static str, Ticks>,
    job: Option<Job>,
    notice: Option<String>,
    error: Option<String>,
    offline: bool,
    dir_pick: FolderPick,
    import_pick: ImportPick,
    shown_dir: Option<(Option<PathBuf>, Option<PathBuf>)>,
    lead_pick: FolderPick,
    expand_once: Option<String>,
    selected: Option<Selection>,
    filter: ListFilter,
    licence_once: Option<&'static str>,
    cache: crate::connectome_cache_ui::CacheManager,
}

impl ConnectomePanel {
    pub fn open(&mut self) {
        self.open = true;
        self.expand_once = std::env::var("DBSLOC_APP_CONNECTOMES_EXPAND")
            .ok()
            .filter(|v| !v.trim().is_empty());
        self.refresh_wanted = true;
    }

    pub fn open_cache_manager(&mut self) {
        self.cache.open();
    }

    /// Looks at the folders on disk again.
    pub fn refresh(&mut self) {
        self.refresh_wanted = true;
    }

    fn resolve_roots(state: &DbslocAppState) -> Result<ConnectomeRoots, String> {
        resolve_with_setting(
            state.stage.connectomes_dir.as_deref(),
            state.stage.leaddbs_connectomes_dir.as_deref(),
        )
        .map_err(|e| e.message)
    }

    fn poll(&mut self, ctx: &egui::Context, state: &mut DbslocAppState) {
        if let Some(dir) = self.dir_pick.poll() {
            state.stage.connectomes_dir = Some(dir);
        }
        if let Some(dir) = self.lead_pick.poll() {
            state.stage.leaddbs_connectomes_dir = Some(dir);
        }
        if let Some((path, archive, target)) = self.import_pick.poll() {
            self.start_import(ctx, state, path, archive, target);
        }
        let dir = (
            state.stage.connectomes_dir.clone(),
            state.stage.leaddbs_connectomes_dir.clone(),
        );
        if self.shown_dir.as_ref() != Some(&dir) {
            self.shown_dir = Some(dir);
            self.refresh_wanted = true;
        }
        if let Some(rx) = &self.listing_rx {
            match rx.try_recv() {
                Ok(listing) => {
                    self.listing = Some(listing);
                    self.listing_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.listing_rx = None,
            }
        }
        self.poll_job();
        if self.refresh_wanted && self.listing_rx.is_none() && self.open {
            self.refresh_wanted = false;
            match Self::resolve_roots(state) {
                Ok(roots) => {
                    let (tx, rx) = mpsc::channel();
                    self.listing_rx = Some(rx);
                    let ctx = ctx.clone();
                    thread::spawn(move || {
                        let _ = tx.send(build_listing(roots));
                        ctx.request_repaint();
                    });
                }
                Err(message) => self.error = Some(message),
            }
        }
    }

    fn poll_job(&mut self) {
        let mut finished: Option<Result<String, ConnectomeError>> = None;
        if let Some(job) = self.job.as_mut() {
            loop {
                match job.rx.try_recv() {
                    Ok(Msg::Event(ev)) => apply_event(job, ev),
                    Ok(Msg::Done(r)) => {
                        finished = Some(r);
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = Some(Err(ConnectomeError::new(
                            ErrorCode::Io,
                            "the worker stopped unexpectedly",
                        )));
                        break;
                    }
                }
            }
        }
        let Some(result) = finished else { return };
        let Some(job) = self.job.take() else { return };
        self.refresh_wanted = true;
        match result {
            Ok(message) => {
                if job.kind == JobKind::Fetch {
                    self.offline = false;
                }
                self.error = None;
                self.notice = Some(message);
            }
            Err(e) if e.is_cancelled() || job.cancelled => {
                self.error = None;
                self.notice = Some(plain_message(&ConnectomeError::cancelled()));
            }
            Err(e) => {
                if e.code == ErrorCode::Host {
                    self.offline = true;
                }
                self.notice = None;
                self.error = Some(plain_message(&e));
            }
        }
    }

    fn start_fetch(
        &mut self,
        ctx: &egui::Context,
        entry: &'static ConnectomeEntry,
        roots: ConnectomeRoots,
        ticks: Ticks,
    ) {
        if self.job.is_some() {
            return;
        }
        self.error = None;
        self.notice = None;
        let acceptance = Acceptance {
            accept_terms: ticks.accept_terms,
            attest_data_use_agreement: ticks.attest,
            via: "gui",
        };
        let name = entry.name.to_owned();
        self.spawn(
            ctx,
            JobKind::Fetch,
            format!("Downloading {name}"),
            Some(entry.id),
            move |cancel, on_event| {
                connectome_fetch::fetch_connectome(
                    entry,
                    &roots,
                    &acceptance,
                    &FetchSettings::default(),
                    cancel,
                    on_event,
                )
                .map(|o| format!("{name} is installed at {}.", o.path.display()))
            },
        );
    }

    fn start_import(
        &mut self,
        ctx: &egui::Context,
        state: &DbslocAppState,
        path: PathBuf,
        archive: bool,
        target: Option<&'static str>,
    ) {
        if self.job.is_some() {
            return;
        }
        let roots = match Self::resolve_roots(state) {
            Ok(r) => r,
            Err(message) => {
                self.error = Some(message);
                return;
            }
        };
        self.error = None;
        self.notice = None;
        let entry = target.and_then(connectome_catalog::lookup);
        let class = entry.map(|e| e.class);
        let acceptance = entry.and_then(|e| {
            let ticks = self.ticks.get(e.id).copied().unwrap_or_default();
            terms_complete(e.access, ticks).then_some(Acceptance {
                accept_terms: true,
                attest_data_use_agreement: ticks.attest,
                via: "gui",
            })
        });
        let label = format!("Importing {}", path.display());
        self.spawn(
            ctx,
            JobKind::Import,
            label,
            target,
            move |cancel, _on_event| {
                let out = if archive {
                    connectome_fetch::import_archive(
                        &roots,
                        &path,
                        class,
                        acceptance.as_ref(),
                        cancel,
                    )
                } else {
                    connectome_fetch::import_folder(&roots, &path, class, acceptance.as_ref())
                }?;
                let mut text = if out.extracted {
                    format!("Extracted the archive to {}.", out.path.display())
                } else {
                    format!("Registered {} where it is.", out.path.display())
                };
                if out.licence_note.is_some() {
                    text.push_str(
                        " No licence record was saved. Tick the boxes of this dataset and import again to record it.",
                    );
                }
                Ok(text)
            },
        );
    }

    fn spawn<F>(
        &mut self,
        ctx: &egui::Context,
        kind: JobKind,
        label: String,
        entry_id: Option<&'static str>,
        job: F,
    ) where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(ConnectomeEvent)) -> Result<String, ConnectomeError>
            + Send
            + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let tx_events = tx.clone();
            let ctx_events = ctx.clone();
            let result = job(worker_cancel.as_ref(), &mut |ev| {
                let _ = tx_events.send(Msg::Event(ev));
                ctx_events.request_repaint();
            });
            let _ = tx.send(Msg::Done(result));
            ctx.request_repaint();
        });
        self.job = Some(Job {
            kind,
            label,
            entry_id,
            rx,
            cancel,
            cancelled: false,
            downloaded: 0,
            total: 0,
            file: None,
            phase: Phase::Downloading,
            retry: None,
            started: Instant::now(),
            first_sample: None,
        });
    }

    fn apply(&mut self, ctx: &egui::Context, state: &mut DbslocAppState, action: Action) {
        match action {
            Action::Download(id) => {
                let Some(entry) = connectome_catalog::lookup(id) else {
                    return;
                };
                match Self::resolve_roots(state) {
                    Ok(roots) => {
                        let ticks = self.ticks.get(id).copied().unwrap_or_default();
                        self.start_fetch(ctx, entry, roots, ticks);
                    }
                    Err(message) => self.error = Some(message),
                }
            }
            Action::Import { archive, target } => self.import_pick.open(ctx, archive, target),
            Action::OpenUrl(url) => {
                if let Err(e) = crate::win32::open_url(&url) {
                    self.error = Some(format!("{e}\nThe address is {url}"));
                }
            }
            Action::Forget(path) => {
                if let Some(listing) = &self.listing {
                    match connectome_fetch::forget_folder(&listing.roots.primary, &path) {
                        Ok(_) => {
                            self.notice = Some(format!(
                                "Forgot {}. The folder itself is untouched.",
                                path.display()
                            ));
                            self.refresh_wanted = true;
                        }
                        Err(e) => self.error = Some(plain_message(&e)),
                    }
                }
            }
            Action::RecordAcceptance(id) => {
                let Some(entry) = connectome_catalog::lookup(id) else {
                    return;
                };
                let ticks = self.ticks.get(id).copied().unwrap_or_default();
                match Self::resolve_roots(state).and_then(|roots| {
                    record_acceptance(&roots.primary, entry, ticks).map_err(|e| plain_message(&e))
                }) {
                    Ok(message) => {
                        self.error = None;
                        self.notice = Some(message);
                        self.refresh_wanted = true;
                    }
                    Err(message) => {
                        self.notice = None;
                        self.error = Some(message);
                    }
                }
            }
            Action::Cancel => {
                if let Some(job) = self.job.as_mut() {
                    job.cancelled = true;
                    job.cancel.store(true, Ordering::SeqCst);
                }
            }
            Action::CheckAgain => self.offline = false,
            Action::Refresh => self.refresh_wanted = true,
            Action::ChangeDirectory => {
                let start = state.stage.connectomes_dir.clone();
                self.dir_pick
                    .open(ctx, "Select the connectomes directory", start);
            }
            Action::UseDefaultDirectory => state.stage.connectomes_dir = None,
            Action::ChangeLeadDbsFolder => {
                let start = state.stage.leaddbs_connectomes_dir.clone();
                self.lead_pick
                    .open(ctx, "Select the Lead-DBS connectomes folder", start);
            }
            Action::ClearLeadDbsFolder => state.stage.leaddbs_connectomes_dir = None,
            Action::ImportLeadDbs => state.leaddbs_import_request = true,
            Action::SetCache(mode) => state.connectome_cache = mode,
            Action::ManageCache => self.cache.open(),
            Action::Reveal(path) => {
                if let Err(e) = crate::panels::reveal_in_file_manager(&path) {
                    self.error = Some(e);
                }
            }
            Action::RevealDirectory => {
                if let Some(listing) = &self.listing {
                    if let Err(e) = crate::panels::reveal_in_file_manager(&listing.roots.primary) {
                        self.error = Some(e);
                    }
                }
            }
        }
    }

    /// Draws the window when it is open and keeps the workers moving either way.
    pub fn show(&mut self, ctx: &egui::Context, state: &mut DbslocAppState) {
        self.poll(ctx, state);
        self.cache.show(ctx);
        if !self.open {
            return;
        }
        let mut open = true;
        let mut actions: Vec<Action> = Vec::new();
        let screen = ctx.content_rect().size();
        egui::Window::new("Connectomes")
            .id(egui::Id::new("connectomes_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(egui::vec2(
                (screen.x * 0.8).min(900.0),
                (screen.y * 0.85).min(760.0),
            ))
            .max_size(screen * 0.95)
            .show(ctx, |ui| {
                self.contents(ui, state, &mut actions);
            });
        if !open {
            self.open = false;
        }
        for a in actions {
            self.apply(ctx, state, a);
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, state: &DbslocAppState, actions: &mut Vec<Action>) {
        let pal = palette(ui);
        let unit = em(ui);
        self.top_bar(ui, state, actions);
        self.status_area(ui, actions);
        ui.separator();
        let Some(listing) = self.listing.take() else {
            ui.label(egui::RichText::new("Reading the folders…").color(pal.text_muted));
            return;
        };
        self.settle_selection(&listing);
        let height = ui.available_height();
        let width = ui.available_width();
        if is_narrow(width, unit) {
            self.filter_chips(ui);
            egui::ScrollArea::vertical()
                .id_salt("connectome_list")
                .max_height((height * 0.38).max(unit * 8.0))
                .auto_shrink([false, true])
                .show(ui, |ui| self.list_pane(ui, &listing));
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("connectome_detail")
                .auto_shrink([false, false])
                .show(ui, |ui| self.detail_pane(ui, &listing, actions));
        } else {
            let list_w = list_width(width, unit);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(list_w, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(list_w, height));
                        self.filter_chips(ui);
                        egui::ScrollArea::vertical()
                            .id_salt("connectome_list")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.list_pane(ui, &listing));
                    },
                );
                ui.separator();
                let rest = ui.available_width();
                ui.allocate_ui_with_layout(
                    egui::vec2(rest, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(rest, height));
                        egui::ScrollArea::vertical()
                            .id_salt("connectome_detail")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.detail_pane(ui, &listing, actions));
                    },
                );
            });
        }
        self.listing = Some(listing);
    }

    fn settle_selection(&mut self, listing: &Listing) {
        if let Some(id) = self.expand_once.take() {
            if let Some(row) = listing.rows.iter().find(|r| r.status.entry.id == id) {
                self.selected = Some(Selection::Entry(row.status.entry.id));
                self.licence_once = Some(row.status.entry.id);
            }
        }
        let valid = match &self.selected {
            Some(Selection::Entry(id)) => listing.rows.iter().any(|r| r.status.entry.id == *id),
            Some(Selection::Other(p)) => listing.others.iter().any(|c| &c.path == p),
            None => false,
        };
        if !valid {
            let installed: Vec<bool> = listing.rows.iter().map(Row::is_installed).collect();
            self.selected = default_selection(&installed)
                .map(|i| Selection::Entry(listing.rows[i].status.entry.id));
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, state: &DbslocAppState, actions: &mut Vec<Action>) {
        let pal = palette(ui);
        let resolved = Self::resolve_roots(state).ok();
        let current = self
            .listing
            .as_ref()
            .map(|l| l.roots.primary.clone())
            .or_else(|| resolved.as_ref().map(|r| r.primary.clone()));
        let external = current_external_source(state.stage.leaddbs_connectomes_dir.as_deref());
        let can_pick = !self.import_pick.is_open() && self.job.is_none();
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Directory").color(pal.text_muted));
            match &current {
                Some(dir) => {
                    ui.add(egui::Label::new(shorten_path(dir, 40)).selectable(false))
                        .on_hover_text(dir.display().to_string());
                }
                None => {
                    ui.label(egui::RichText::new("not available").color(pal.err));
                }
            }
            info_icon(ui, DIRECTORY_INFO);
            if ui
                .add_enabled(!self.dir_pick.is_open(), egui::Button::new("Change…").small())
                .on_hover_text("Choose where connectomes are stored")
                .clicked()
            {
                actions.push(Action::ChangeDirectory);
            }
            if state.stage.connectomes_dir.is_some()
                && ui
                    .small_button("Use default")
                    .on_hover_text("Go back to the per-user folder of the application")
                    .clicked()
            {
                actions.push(Action::UseDefaultDirectory);
            }
            if current.is_some()
                && open_folder_button(ui, "Open the Connectomes directory in the file manager")
            {
                actions.push(Action::RevealDirectory);
            }
            ui.add_enabled_ui(can_pick, |ui| {
                ui.menu_button("Import…", |ui| {
                    if ui
                        .button("Folder…")
                        .on_hover_text(
                            "Register a connectome folder you already have, where it is. Nothing is copied.",
                        )
                        .clicked()
                    {
                        actions.push(Action::Import {
                            archive: false,
                            target: None,
                        });
                        ui.close();
                    }
                    if ui
                        .button("Archive…")
                        .on_hover_text(
                            "Extract a downloaded .zip or .tar file into the Connectomes directory",
                        )
                        .clicked()
                    {
                        actions.push(Action::Import {
                            archive: true,
                            target: None,
                        });
                        ui.close();
                    }
                    if ui
                        .button("From a Lead-DBS installation…")
                        .on_hover_text(
                            "Use the atlases and connectomes of a Lead-DBS installation, where they are or as copies",
                        )
                        .clicked()
                    {
                        actions.push(Action::ImportLeadDbs);
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Add a connectome you already have");
            });
            let cache_mode = state.connectome_cache;
            ui.menu_button("Cache…", |ui| {
                for mode in crate::connectome_cache_ui::CacheMode::ALL {
                    if ui
                        .radio(cache_mode == mode, mode.label())
                        .on_hover_text(mode.tip())
                        .clicked()
                        && cache_mode != mode
                    {
                        actions.push(Action::SetCache(mode));
                    }
                }
                ui.separator();
                if ui
                    .button("Manage cache…")
                    .on_hover_text("List the ready-to-use copies on this computer and delete some")
                    .clicked()
                {
                    actions.push(Action::ManageCache);
                    ui.close();
                }
            })
            .response
            .on_hover_text(format!(
                "Ready-to-use copies: {}. {}",
                cache_mode.label(),
                crate::connectome_cache_ui::MODE_INFO
            ));
            if ui
                .add_enabled(self.listing_rx.is_none(), egui::Button::new("Refresh").small())
                .on_hover_text("Look at the folders on disk again")
                .clicked()
            {
                actions.push(Action::Refresh);
            }
            if self.listing_rx.is_some() {
                ui.add(egui::Spinner::new().size(em(ui)));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Lead-DBS connectomes folder").color(pal.text_muted));
            let text = leaddbs_folder_text(&external);
            ui.add(egui::Label::new(shorten_path(Path::new(&text), 60)).selectable(false))
                .on_hover_text(text);
            info_icon(ui, LEADDBS_ROOT_INFO);
            if ui
                .add_enabled(
                    !self.lead_pick.is_open(),
                    egui::Button::new("Change…").small(),
                )
                .on_hover_text("Choose a Lead-DBS connectomes folder to read where it is")
                .clicked()
            {
                actions.push(Action::ChangeLeadDbsFolder);
            }
            if state.stage.leaddbs_connectomes_dir.is_some()
                && ui
                    .small_button("Clear")
                    .on_hover_text(
                        "Stop using this folder; the environment variable, if set, applies again",
                    )
                    .clicked()
            {
                actions.push(Action::ClearLeadDbsFolder);
            }
        });
    }

    fn filter_chips(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for f in ListFilter::ALL {
                if ui
                    .selectable_label(self.filter == f, f.label())
                    .on_hover_text(f.tip())
                    .clicked()
                {
                    self.filter = f;
                }
            }
        });
        ui.add_space(em(ui) * 0.2);
    }

    fn list_pane(&mut self, ui: &mut egui::Ui, listing: &Listing) {
        let pal = palette(ui);
        let unit = em(ui);
        let items: Vec<(bool, ConnectomeClass)> = listing
            .rows
            .iter()
            .map(|r| (r.is_installed(), r.status.entry.class))
            .collect();
        let (installed, available) = split_rows(&items, self.filter);
        let others: Vec<usize> = listing
            .others
            .iter()
            .enumerate()
            .filter(|(_, c)| other_visible(self.filter, !c.missing, c.class))
            .map(|(i, _)| i)
            .collect();
        if installed.is_empty() && available.is_empty() && others.is_empty() {
            ui.label(egui::RichText::new("Nothing matches this filter.").color(pal.text_muted));
        }
        for (title, group) in [("Installed", &installed), ("Available", &available)] {
            if group.is_empty() {
                continue;
            }
            list_heading(ui, title);
            for &i in group {
                let row = &listing.rows[i];
                let entry = row.status.entry;
                let dot = match row.status.state {
                    ConnectomeState::Installed(_) => pal.ok,
                    ConnectomeState::Partial { .. } => pal.warn,
                    ConnectomeState::Available { .. } => pal.text_muted,
                };
                let selected = self.selected == Some(Selection::Entry(entry.id));
                let sub = row_subtitle(
                    entry.class.kind_label(),
                    &row.status.state,
                    entry.total_bytes(),
                );
                let resp = list_row(ui, ("conn_row", entry.id), selected, entry.name, &sub, dot)
                    .on_hover_text(format!(
                        "{}\n{}",
                        entry.citation,
                        badge_text(&row.status.state, row.origin)
                    ));
                if resp.clicked() {
                    self.selected = Some(Selection::Entry(entry.id));
                }
            }
        }
        if !others.is_empty() {
            list_heading(ui, "Other folders");
            for i in others {
                let c = &listing.others[i];
                let dot = if c.missing { pal.err } else { pal.ok };
                let selected = self.selected == Some(Selection::Other(c.path.clone()));
                let sub = format!("{} · {}", c.class.kind_label(), c.space);
                let resp = list_row(
                    ui,
                    ("conn_other", c.path.display().to_string()),
                    selected,
                    &c.name,
                    &sub,
                    dot,
                )
                .on_hover_text(c.path.display().to_string());
                if resp.clicked() {
                    self.selected = Some(Selection::Other(c.path.clone()));
                }
            }
        }
        ui.add_space(unit * 0.5);
    }

    fn detail_pane(&mut self, ui: &mut egui::Ui, listing: &Listing, actions: &mut Vec<Action>) {
        let pal = palette(ui);
        match self.selected.clone() {
            Some(Selection::Entry(id)) => {
                if let Some(row) = listing.rows.iter().find(|r| r.status.entry.id == id) {
                    self.entry_detail(ui, listing, row, actions);
                }
            }
            Some(Selection::Other(path)) => {
                if let Some(c) = listing.others.iter().find(|c| c.path == path) {
                    self.other_detail(ui, c, actions);
                }
            }
            None => {
                ui.label(egui::RichText::new("Select a connectome.").color(pal.text_muted));
            }
        }
    }

    fn other_detail(&self, ui: &mut egui::Ui, c: &InstalledConnectome, actions: &mut Vec<Action>) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.add(egui::Label::new(egui::RichText::new(&c.name).heading()).wrap());
        ui.add_space(unit * 0.3);
        ui.horizontal_wrapped(|ui| {
            if !c.missing && open_folder_button(ui, "Open the folder in the file manager") {
                actions.push(Action::Reveal(c.path.clone()));
            }
            if c.root_kind == RootKind::Registered
                && ui
                    .button("Forget")
                    .on_hover_text(
                        "Remove the folder from the list. The folder itself is not deleted.",
                    )
                    .clicked()
            {
                actions.push(Action::Forget(c.path.clone()));
            }
        });
        ui.add_space(unit * 0.3);
        let status = if c.missing {
            "Missing"
        } else {
            match c.root_kind {
                RootKind::External => "External Lead-DBS folder",
                RootKind::Registered => "Registered folder",
                RootKind::Primary => "Installed",
            }
        };
        egui::Grid::new("connectome_other_facts")
            .num_columns(2)
            .spacing([unit, unit * 0.2])
            .show(ui, |ui| {
                fact(ui, "Type", c.class.kind_label());
                fact(ui, "Space", &c.space);
                if let Some(v) = &c.version {
                    fact(ui, "Version", v);
                }
                fact_path(ui, "Folder", &c.path.display().to_string());
                ui.label(egui::RichText::new("State").color(pal.text_muted));
                ui.label(egui::RichText::new(status).color(if c.missing {
                    pal.err
                } else {
                    pal.ok
                }));
                ui.end_row();
            });
    }

    fn status_area(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let pal = palette(ui);
        let unit = em(ui);
        if let Some(job) = &self.job {
            callout(ui, pal.accent, &job.label, |ui| {
                let (frac, text) = match job.phase {
                    Phase::Downloading if job.kind == JobKind::Fetch => {
                        let frac = if job.total > 0 {
                            (job.downloaded as f32 / job.total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        (frac, progress_text(job.downloaded, job.total, job.rate()))
                    }
                    Phase::Downloading => (1.0, "Working on the folder…".to_owned()),
                    Phase::Verifying => (1.0, "Checking the download…".to_owned()),
                    Phase::Extracting => (1.0, "Extracting the files…".to_owned()),
                };
                ui.add(egui::ProgressBar::new(frac).text(text));
                if let Some((n, count)) = job.file.filter(|(_, c)| *c > 1) {
                    ui.label(
                        egui::RichText::new(format!("File {n} of {count}")).color(pal.text_muted),
                    );
                }
                if let Some(reason) = &job.retry {
                    ui.add(egui::Label::new(egui::RichText::new(reason).color(pal.warn)).wrap());
                }
                ui.add_space(unit * 0.2);
                if ui
                    .add_enabled(!job.cancelled, egui::Button::new("Cancel"))
                    .on_hover_text(
                        "Stop now. A partial download is kept and continues the next time.",
                    )
                    .clicked()
                {
                    actions.push(Action::Cancel);
                }
            });
        }
        if let Some(notice) = self.notice.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(notice).color(pal.ok)).wrap());
                if ui
                    .small_button("Dismiss")
                    .on_hover_text("Hide this message")
                    .clicked()
                {
                    self.notice = None;
                }
            });
        }
        if let Some(error) = self.error.clone() {
            callout(ui, pal.err, "Something went wrong", |ui| {
                ui.add(egui::Label::new(egui::RichText::new(error.as_str()).color(pal.err)).wrap());
                ui.horizontal(|ui| {
                    if ui
                        .button("Copy")
                        .on_hover_text("Copy the full message to the clipboard")
                        .clicked()
                    {
                        ui.ctx().copy_text(error.clone());
                    }
                    if ui
                        .button("Dismiss")
                        .on_hover_text("Hide this message")
                        .clicked()
                    {
                        self.error = None;
                    }
                });
            });
        }
    }

    fn entry_detail(
        &mut self,
        ui: &mut egui::Ui,
        listing: &Listing,
        row: &Row,
        actions: &mut Vec<Action>,
    ) {
        let pal = palette(ui);
        let unit = em(ui);
        let entry = row.status.entry;
        let installed = row.is_installed();
        let partial = match row.status.state {
            ConnectomeState::Partial { bytes, .. } => bytes,
            _ => 0,
        };
        let ticks = self.ticks.entry(entry.id).or_default();
        let controls = card_controls(&CardContext {
            access: entry.access,
            installed,
            ticks: *ticks,
            download_busy: self.job.as_ref().is_some_and(|j| j.kind == JobKind::Fetch),
            import_busy: self.job.is_some() || self.import_pick.is_open(),
            offline: self.offline,
            disk_short: disk_short(listing.free, entry, partial),
        });

        ui.add(egui::Label::new(egui::RichText::new(entry.name).heading()).wrap());
        ui.add(
            egui::Label::new(
                egui::RichText::new(entry.citation)
                    .small()
                    .color(pal.text_muted),
            )
            .wrap(),
        );
        ui.add_space(unit * 0.4);

        let mut disabled_reason: Option<&'static str> = None;
        ui.horizontal_wrapped(|ui| {
            let label = if partial > 0 {
                format!(
                    "Resume download ({})",
                    bytes_label(entry.total_bytes().saturating_sub(partial))
                )
            } else {
                format!("Download ({})", bytes_label(entry.total_bytes()))
            };
            match controls.download {
                DownloadControl::Hidden if !installed && entry.access != Access::Dua => {
                    let hint = missing_acceptance_hint(entry.access, *ticks).unwrap_or_default();
                    ui.add_enabled(false, egui::Button::new(label))
                        .on_disabled_hover_text(format!(
                            "Read and accept the terms below first. {hint}"
                        ));
                }
                DownloadControl::Hidden => {}
                DownloadControl::Enabled => {
                    if ui
                        .button(label)
                        .on_hover_text("Download from the original host to the Connectomes directory")
                        .clicked()
                    {
                        actions.push(Action::Download(entry.id));
                    }
                }
                DownloadControl::Disabled(reason) => {
                    ui.add_enabled(false, egui::Button::new(label))
                        .on_disabled_hover_text(reason);
                    disabled_reason = Some(reason);
                }
            }
            if ui
                .add_enabled(controls.choose_folder, egui::Button::new("Choose folder…"))
                .on_hover_text(
                    "Register a folder of this dataset that you obtained yourself, for example one that Lead-DBS installed. It stays where it is.",
                )
                .clicked()
            {
                actions.push(Action::Import {
                    archive: false,
                    target: Some(entry.id),
                });
            }
            if ui
                .add_enabled(controls.import_archive, egui::Button::new("Import archive…"))
                .on_hover_text(
                    "Extract an archive of this dataset that you downloaded yourself. When the file name is the published one, its size and checksums are checked first.",
                )
                .clicked()
            {
                actions.push(Action::Import {
                    archive: true,
                    target: Some(entry.id),
                });
            }
            if let ConnectomeState::Installed(p) = &row.status.state {
                if open_folder_button(ui, "Open the folder in the file manager") {
                    actions.push(Action::Reveal(p.clone()));
                }
                if row.origin == Some(RootKind::Registered)
                    && ui
                        .button("Forget")
                        .on_hover_text(
                            "Remove the folder from the list. The folder itself is not deleted.",
                        )
                        .clicked()
                {
                    actions.push(Action::Forget(p.clone()));
                }
            }
            if ui
                .link("Website")
                .on_hover_text(entry.dataset_url)
                .clicked()
            {
                actions.push(Action::OpenUrl(entry.dataset_url.to_owned()));
            }
        });
        if let Some(reason) = disabled_reason {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(reason).small().color(pal.warn)).wrap(),
                );
                if self.offline
                    && ui
                        .small_button("Check again")
                        .on_hover_text("Allow another download attempt")
                        .clicked()
                {
                    actions.push(Action::CheckAgain);
                }
            });
        }
        if self
            .job
            .as_ref()
            .is_some_and(|j| j.entry_id == Some(entry.id))
        {
            ui.label(
                egui::RichText::new("This dataset is being processed above.")
                    .small()
                    .color(pal.text_muted),
            );
        }
        ui.add_space(unit * 0.4);

        egui::Grid::new(("connectome_facts", entry.id))
            .num_columns(2)
            .spacing([unit, unit * 0.2])
            .show(ui, |ui| {
                fact(ui, "Type", entry.class.kind_label());
                fact(ui, "Space", entry.space);
                fact(ui, "Cohort", entry.cohort);
                fact(ui, "Version", entry.version);
                fact(
                    ui,
                    "Licence",
                    &format!("{} · {}", entry.licence_name, access_text(entry.access)),
                );
                fact(ui, "Size", &bytes_label(entry.total_bytes()));
                match &row.status.state {
                    ConnectomeState::Installed(p) => {
                        let place = match row.origin {
                            Some(RootKind::External) => "External Lead-DBS folder",
                            Some(RootKind::Registered) => "Registered folder",
                            _ => "Installed",
                        };
                        fact(ui, "State", place);
                        fact_path(ui, "Folder", &p.display().to_string());
                    }
                    other => fact(
                        ui,
                        "State",
                        &card_state_text(entry.access, other, row.origin),
                    ),
                }
                if let Some(v) = &row.version {
                    fact(ui, "Installed version", v);
                }
                match listing.records.get(entry.id) {
                    Some(r) if r.stale => fact(
                        ui,
                        "Licence record",
                        &format!(
                            "accepted {}; the text changed since, you are asked again before the next download",
                            date_of(&r.accepted_at)
                        ),
                    ),
                    Some(r) => fact(
                        ui,
                        "Licence record",
                        &format!("accepted {}", date_of(&r.accepted_at)),
                    ),
                    None if installed => fact(ui, "Licence record", "No licence record"),
                    None => {}
                }
            });
        ui.add_space(unit * 0.4);

        if entry.access == Access::Dua && controls.show_instructions && !installed {
            how_section(ui, entry, actions);
        }

        let force_licence = self.licence_once == Some(entry.id);
        if force_licence {
            self.licence_once = None;
        }
        let record_state = match listing.records.get(entry.id) {
            None => RecordState::Missing,
            Some(r) if r.stale => RecordState::Stale,
            Some(_) => RecordState::Current,
        };
        let record_control = record_control(entry.access, installed, record_state, *ticks);
        if !installed || record_control != RecordControl::Hidden {
            let mut header =
                egui::CollapsingHeader::new(egui::RichText::new("Licence and terms").strong())
                    .id_salt(("connectome_licence_section", entry.id))
                    .default_open(
                        record_control != RecordControl::Hidden
                            || licence_open_by_default(
                                installed,
                                terms_complete(entry.access, *ticks),
                            ),
                    );
            if force_licence {
                header = header.open(Some(true));
            }
            header.show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(entry.licence_name).small());
                    ui.label(
                        egui::RichText::new(format!("fetched {}", entry.terms_fetched))
                            .small()
                            .color(pal.text_muted),
                    );
                    if !entry.licence_url.is_empty()
                        && ui
                            .link(egui::RichText::new("Open the source").small())
                            .on_hover_text(entry.licence_url)
                            .clicked()
                    {
                        actions.push(Action::OpenUrl(entry.licence_url.to_owned()));
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt(("connectome_licence", entry.id))
                    .max_height(unit * 9.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let font = egui::TextStyle::Small.resolve(ui.style());
                        let mut job = egui::text::LayoutJob::simple(
                            entry.licence_text.to_owned(),
                            font,
                            ui.visuals().text_color(),
                            ui.available_width(),
                        );
                        job.wrap.break_anywhere = true;
                        ui.add(egui::Label::new(job));
                    });
                ui.add_space(unit * 0.2);
                ui.checkbox(&mut ticks.accept_terms, "I have read and accept these terms")
                    .on_hover_text(if installed {
                        "Saved as a licence record in the Connectomes directory. Nothing is sent anywhere"
                    } else if entry.access == Access::Dua {
                        "Recorded with the dataset when you import it"
                    } else {
                        "Required before a download"
                    });
                if controls.show_attestation {
                    ui.checkbox(&mut ticks.attest, entry.attestation_sentence())
                        .on_hover_text(if installed {
                            "Saved with the licence record. Only people who hold their own approved data use agreement may use this dataset"
                        } else {
                            "Recorded with the dataset when you import it. Only people who hold their own approved data use agreement may use this dataset"
                        });
                }
                match record_control {
                    RecordControl::Hidden => {}
                    RecordControl::Enabled => {
                        if ui
                            .button("Record acceptance")
                            .on_hover_text(
                                "Save your acceptance of these terms as a licence record in the Connectomes directory",
                            )
                            .clicked()
                        {
                            actions.push(Action::RecordAcceptance(entry.id));
                        }
                    }
                    RecordControl::Disabled(reason) => {
                        ui.add_enabled(false, egui::Button::new("Record acceptance"))
                            .on_disabled_hover_text(reason);
                    }
                }
                if installed {
                    ui.label(
                        egui::RichText::new(
                            "This connectome is already installed. Recording your acceptance changes nothing about the data.",
                        )
                        .small()
                        .color(pal.text_muted),
                    );
                    return;
                }
                if let Some(hint) = missing_acceptance_hint(entry.access, *ticks) {
                    ui.label(egui::RichText::new(hint).small().color(pal.text_muted));
                }
                if partial > 0 && entry.access != Access::Dua {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "A partial download of {} is kept. The download continues from there.",
                                bytes_label(partial)
                            ))
                            .color(pal.warn),
                        )
                        .wrap(),
                    );
                }
                let needed = connectome_fetch::required_free_bytes(entry, GB);
                let free_text = match listing.free {
                    Some(f) => format!(
                        "Free disk space: {}. Needed: {}.",
                        bytes_label(f),
                        bytes_label(needed.saturating_sub(partial))
                    ),
                    None => format!("Needed disk space: {}.", bytes_label(needed)),
                };
                ui.label(egui::RichText::new(free_text).small().color(pal.text_muted));
            });
        } else if !entry.licence_url.is_empty()
            && ui
                .link("Licence page")
                .on_hover_text(entry.licence_url)
                .clicked()
        {
            actions.push(Action::OpenUrl(entry.licence_url.to_owned()));
        }

        if !entry.acknowledgement.is_empty() {
            egui::CollapsingHeader::new(egui::RichText::new("Required acknowledgement").strong())
                .id_salt(("connectome_ack_section", entry.id))
                .default_open(false)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(entry.acknowledgement).small()).wrap(),
                    );
                    if ui
                        .button("Copy")
                        .on_hover_text("Copy the acknowledgement sentence")
                        .clicked()
                    {
                        ui.ctx().copy_text(entry.acknowledgement.to_owned());
                    }
                });
        }

        if entry.access != Access::Dua && controls.show_instructions && !installed {
            how_section(ui, entry, actions);
        }
        ui.add_space(unit * 0.5);
    }
}

fn how_section(ui: &mut egui::Ui, entry: &ConnectomeEntry, actions: &mut Vec<Action>) {
    if entry.instructions.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(egui::RichText::new("How to get this dataset").strong())
        .id_salt(("connectome_how_section", entry.id))
        .default_open(how_section_default_open(entry.access))
        .show(ui, |ui| {
            ui.add(egui::Label::new(entry.instructions).wrap());
            for (i, s) in entry.steps.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(step_line(i, s));
                    if let Some(url) = s.url {
                        if ui.link("Open the page").on_hover_text(url).clicked() {
                            actions.push(Action::OpenUrl(url.to_owned()));
                        }
                    }
                });
            }
        });
}

fn apply_event(job: &mut Job, ev: ConnectomeEvent) {
    match ev {
        ConnectomeEvent::Started { total_bytes, .. } => {
            job.total = total_bytes;
            job.first_sample = None;
        }
        ConnectomeEvent::FileStarted { index, count, .. } => {
            job.file = Some((index, count));
            job.phase = Phase::Downloading;
            job.retry = None;
            job.first_sample = None;
        }
        ConnectomeEvent::Progress { downloaded, total } => {
            job.downloaded = downloaded;
            job.total = total;
            job.phase = Phase::Downloading;
            job.retry = None;
            if job.first_sample.is_none() {
                job.first_sample = Some((job.started.elapsed().as_secs_f64(), downloaded));
            }
        }
        ConnectomeEvent::Retry {
            attempt,
            wait_secs,
            reason,
        } => {
            job.retry = Some(format!(
                "Connection problem, trying again in {wait_secs} s (attempt {attempt}): {reason}"
            ));
        }
        ConnectomeEvent::Verifying => job.phase = Phase::Verifying,
        ConnectomeEvent::Extracting => job.phase = Phase::Extracting,
        ConnectomeEvent::Done { .. } => {}
    }
}

fn fact(ui: &mut egui::Ui, label: &str, value: &str) {
    let pal = palette(ui);
    ui.label(egui::RichText::new(label).color(pal.text_muted));
    ui.add(egui::Label::new(value).wrap());
    ui.end_row();
}

fn fact_path(ui: &mut egui::Ui, label: &str, value: &str) {
    let pal = palette(ui);
    ui.label(egui::RichText::new(label).color(pal.text_muted));
    ui.add(egui::Label::new(value).truncate())
        .on_hover_text(value);
    ui.end_row();
}

fn list_heading(ui: &mut egui::Ui, text: &str) {
    let pal = palette(ui);
    ui.add_space(em(ui) * 0.4);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(pal.text_muted),
    );
}

fn list_row(
    ui: &mut egui::Ui,
    id: impl egui::AsIdSalt,
    selected: bool,
    title: &str,
    sub: &str,
    dot: egui::Color32,
) -> egui::Response {
    let pal = palette(ui);
    let unit = em(ui);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), unit * 2.6),
        egui::Sense::hover(),
    );
    if ui.is_rect_visible(rect) {
        let hovered = ui.rect_contains_pointer(rect);
        let fill = if selected {
            pal.accent.gamma_multiply(0.18)
        } else if hovered {
            pal.surface_raised
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 4, fill);
        if selected {
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(unit * 0.2, rect.height()));
            ui.painter().rect_filled(bar, 2, pal.accent);
        }
        ui.painter().circle_filled(
            egui::pos2(rect.left() + unit * 0.9, rect.center().y),
            unit * 0.28,
            dot,
        );
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + unit * 1.6, rect.top() + unit * 0.3),
            egui::pos2(rect.right() - unit * 0.4, rect.bottom() - unit * 0.2),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(
                egui::Label::new(egui::RichText::new(title).strong())
                    .truncate()
                    .selectable(false),
            );
            ui.add(
                egui::Label::new(egui::RichText::new(sub).small().color(pal.text_muted))
                    .truncate()
                    .selectable(false),
            );
        });
    }
    ui.interact(rect, ui.id().with(id), egui::Sense::click())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx(access: Access, accept: bool, attest: bool) -> CardContext {
        CardContext {
            access,
            installed: false,
            ticks: Ticks {
                accept_terms: accept,
                attest,
            },
            download_busy: false,
            import_busy: false,
            offline: false,
            disk_short: false,
        }
    }

    const CLASSES: [Access; 3] = [Access::Open, Access::CohortTerms, Access::Dua];

    #[test]
    fn download_button_follows_access_class_and_ticks() {
        for access in CLASSES {
            for accept in [false, true] {
                for attest in [false, true] {
                    let c = card_controls(&ctx(access, accept, attest));
                    let expected = match access {
                        Access::Dua => false,
                        _ => accept,
                    };
                    assert_eq!(
                        c.download == DownloadControl::Enabled,
                        expected,
                        "{access:?} accept {accept} attest {attest}"
                    );
                    if !expected {
                        assert_eq!(c.download, DownloadControl::Hidden);
                    }
                }
            }
        }
    }

    #[test]
    fn a_dua_entry_never_gets_a_download_button_whatever_is_ticked() {
        for (accept, attest) in [(false, false), (true, false), (false, true), (true, true)] {
            let c = card_controls(&ctx(Access::Dua, accept, attest));
            assert_eq!(c.download, DownloadControl::Hidden);
            assert!(c.show_instructions);
            assert!(c.show_attestation);
            assert!(c.choose_folder);
            assert!(c.import_archive);
        }
        let mut busy = ctx(Access::Dua, true, true);
        busy.download_busy = true;
        busy.offline = true;
        busy.disk_short = true;
        assert_eq!(card_controls(&busy).download, DownloadControl::Hidden);
    }

    #[test]
    fn an_import_only_card_says_so_and_opens_its_steps() {
        let available = ConnectomeState::Available { size: 1 };
        assert_eq!(
            card_state_text(Access::Dua, &available, None),
            "Import only: needs your own data access"
        );
        assert_eq!(
            card_state_text(Access::Open, &available, None),
            "Not installed"
        );
        assert!(how_section_default_open(Access::Dua));
        assert!(!how_section_default_open(Access::Open));
        let step = Step {
            text: "Read it.",
            url: None,
        };
        assert_eq!(step_line(0, &step), "1. Read it.");
        assert_eq!(step_line(4, &step), "5. Read it.");
    }

    #[test]
    fn the_dua_hint_says_that_importing_works_without_the_boxes() {
        let none = Ticks::default();
        let hint = missing_acceptance_hint(Access::Dua, none).unwrap();
        assert!(hint.contains("Importing works without the boxes"));
        let both = Ticks {
            accept_terms: true,
            attest: true,
        };
        assert!(missing_acceptance_hint(Access::Dua, both)
            .unwrap()
            .contains("recorded"));
    }

    #[test]
    fn only_dua_entries_ask_for_the_attestation() {
        assert!(!card_controls(&ctx(Access::Open, true, false)).show_attestation);
        assert!(!card_controls(&ctx(Access::CohortTerms, true, false)).show_attestation);
        assert!(card_controls(&ctx(Access::Dua, false, false)).show_attestation);
    }

    #[test]
    fn instructions_and_folder_import_are_always_offered() {
        for access in CLASSES {
            let c = card_controls(&ctx(access, false, false));
            assert!(c.show_instructions);
            assert!(c.choose_folder);
        }
    }

    #[test]
    fn busy_offline_and_full_disk_disable_the_button_with_a_reason() {
        let mut c = ctx(Access::Open, true, false);
        c.download_busy = true;
        assert_eq!(
            card_controls(&c).download,
            DownloadControl::Disabled(BUSY_REASON)
        );
        c.download_busy = false;
        c.offline = true;
        assert_eq!(
            card_controls(&c).download,
            DownloadControl::Disabled(OFFLINE_REASON)
        );
        c.offline = false;
        c.disk_short = true;
        assert_eq!(
            card_controls(&c).download,
            DownloadControl::Disabled(DISK_REASON)
        );
    }

    #[test]
    fn an_installed_entry_has_no_download_button() {
        let mut c = ctx(Access::Open, true, true);
        c.installed = true;
        assert_eq!(card_controls(&c).download, DownloadControl::Hidden);
    }

    #[test]
    fn the_record_section_is_shown_for_installed_terms_entries_without_a_current_record() {
        for access in CLASSES {
            for installed in [false, true] {
                for record in [
                    RecordState::Missing,
                    RecordState::Current,
                    RecordState::Stale,
                ] {
                    let expected =
                        installed && access != Access::Open && record != RecordState::Current;
                    assert_eq!(
                        acceptance_record_needed(access, installed, record),
                        expected,
                        "{access:?} installed {installed} {record:?}"
                    );
                    let control = record_control(access, installed, record, Ticks::default());
                    assert_eq!(control == RecordControl::Hidden, !expected);
                }
            }
        }
    }

    #[test]
    fn the_record_button_needs_the_ticks_of_its_access_class_and_says_why_not() {
        let state = RecordState::Missing;
        let none = Ticks::default();
        let terms = Ticks {
            accept_terms: true,
            attest: false,
        };
        let attest_only = Ticks {
            accept_terms: false,
            attest: true,
        };
        let both = Ticks {
            accept_terms: true,
            attest: true,
        };
        assert_eq!(
            record_control(Access::CohortTerms, true, state, none),
            RecordControl::Disabled(RECORD_TERMS_REASON)
        );
        assert_eq!(
            record_control(Access::CohortTerms, true, state, terms),
            RecordControl::Enabled
        );
        assert_eq!(
            record_control(Access::Dua, true, state, none),
            RecordControl::Disabled(RECORD_TERMS_REASON)
        );
        assert_eq!(
            record_control(Access::Dua, true, state, attest_only),
            RecordControl::Disabled(RECORD_TERMS_REASON)
        );
        assert_eq!(
            record_control(Access::Dua, true, state, terms),
            RecordControl::Disabled(RECORD_ATTEST_REASON)
        );
        assert_eq!(
            record_control(Access::Dua, true, RecordState::Stale, both),
            RecordControl::Enabled
        );
        assert_eq!(
            record_control(Access::Open, true, state, both),
            RecordControl::Hidden
        );
    }

    fn entry_of(access: Access) -> &'static ConnectomeEntry {
        connectome_catalog::catalogue()
            .iter()
            .find(|e| e.access == access)
            .unwrap()
    }

    #[test]
    fn recording_an_acceptance_writes_a_gui_record_with_the_attestation_only_for_dua() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_acc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let both = Ticks {
            accept_terms: true,
            attest: true,
        };

        let dua = entry_of(Access::Dua);
        let message = record_acceptance(&dir, dua, both).unwrap();
        assert_eq!(
            message,
            format!("Recorded your acceptance of the terms of {}.", dua.name)
        );
        let rec = connectome_fetch::read_licence_record(&dir, dua.id).unwrap();
        assert_eq!(rec["accepted_via"], "gui");
        assert_eq!(rec["attestation"], dua.attestation_sentence());
        assert!(!connectome_fetch::licence_record_is_stale(&rec, dua));

        let cohort = entry_of(Access::CohortTerms);
        let terms = Ticks {
            accept_terms: true,
            attest: true,
        };
        record_acceptance(&dir, cohort, terms).unwrap();
        let rec = connectome_fetch::read_licence_record(&dir, cohort.id).unwrap();
        assert_eq!(rec["accepted_via"], "gui");
        assert!(rec["attestation"].is_null());

        assert!(record_acceptance(&dir, dua, Ticks::default()).is_err());
        assert!(record_acceptance(
            &dir,
            dua,
            Ticks {
                accept_terms: true,
                attest: false
            }
        )
        .is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_record_action_writes_the_record_and_shows_a_notice() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_act_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = entry_of(Access::CohortTerms);
        let mut state = DbslocAppState::default();
        state.stage.connectomes_dir = Some(dir.clone());
        let mut panel = ConnectomePanel::default();
        let ctx = egui::Context::default();
        panel.ticks.insert(
            entry.id,
            Ticks {
                accept_terms: true,
                attest: false,
            },
        );
        panel.apply(&ctx, &mut state, Action::RecordAcceptance(entry.id));
        assert!(panel.error.is_none());
        assert!(panel.notice.as_deref().unwrap().contains(entry.name));
        assert!(panel.refresh_wanted);
        assert!(connectome_fetch::read_licence_record(&dir, entry.id).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_window_draws_an_installed_connectome_without_a_licence_record() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_inst_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = connectome_catalog::catalogue()
            .iter()
            .find(|e| e.access == Access::Dua && e.class == ConnectomeClass::Dmri)
            .unwrap();
        let folder = dir.join(entry.class.dir_name()).join(entry.id);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("data.mat"), b"x").unwrap();
        std::fs::write(
            folder.join(connectome_fetch::INSTALL_RECORD),
            format!("{{\"id\":\"{}\"}}", entry.id),
        )
        .unwrap();
        let mut state = DbslocAppState::default();
        state.stage.connectomes_dir = Some(dir.clone());
        let mut panel = ConnectomePanel::default();
        panel.open();
        panel.expand_once = Some(entry.id.to_owned());
        let ctx = egui::Context::default();
        for _ in 0..30 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                panel.show(ui.ctx(), &mut state);
            })
            .textures_delta
            .clear();
            thread::sleep(std::time::Duration::from_millis(20));
        }
        let listing = panel.listing.as_ref().unwrap();
        let row = listing
            .rows
            .iter()
            .find(|r| r.status.entry.id == entry.id)
            .unwrap();
        assert!(row.is_installed());
        assert!(!listing.records.contains_key(entry.id));
        assert_eq!(panel.selected, Some(Selection::Entry(entry.id)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_import_in_progress_disables_the_folder_buttons() {
        let mut c = ctx(Access::Dua, false, false);
        c.import_busy = true;
        let controls = card_controls(&c);
        assert!(!controls.choose_folder);
        assert!(!controls.import_archive);
    }

    #[test]
    fn the_hint_names_what_is_missing() {
        let none = Ticks::default();
        assert!(missing_acceptance_hint(Access::Open, none).is_some());
        let accepted = Ticks {
            accept_terms: true,
            attest: false,
        };
        assert_eq!(missing_acceptance_hint(Access::Open, accepted), None);
        assert!(missing_acceptance_hint(Access::Dua, accepted).is_some());
    }

    #[test]
    fn every_catalogue_entry_is_listed_and_dua_entries_carry_instructions_and_a_link() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_panel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let listing = build_listing(ConnectomeRoots::from_primary(&dir));
        assert_eq!(listing.rows.len(), connectome_catalog::catalogue().len());
        let dua: Vec<_> = listing
            .rows
            .iter()
            .filter(|r| r.status.entry.access == Access::Dua)
            .collect();
        assert!(dua.len() >= 3);
        for r in &dua {
            let e = r.status.entry;
            assert!(!e.instructions.is_empty());
            assert!(e.steps.len() >= 4);
            assert!(e
                .steps
                .iter()
                .all(|s| s.url.is_none_or(|u| u.starts_with("https://"))));
            assert!(e.dataset_url.starts_with("https://"));
            let c = card_controls(&CardContext {
                access: e.access,
                installed: false,
                ticks: Ticks::default(),
                download_busy: false,
                import_busy: false,
                offline: false,
                disk_short: false,
            });
            assert!(c.show_instructions);
            assert_eq!(c.download, DownloadControl::Hidden);
            assert!(matches!(r.status.state, ConnectomeState::Available { .. }));
        }
        assert!(listing
            .rows
            .iter()
            .any(|r| r.status.entry.access == Access::CohortTerms));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_licence_record_shows_its_date() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_rec_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = connectome_catalog::catalogue()
            .iter()
            .find(|e| e.access == Access::Open)
            .unwrap();
        connectome_fetch::write_licence_record(&dir, entry, "gui", None).unwrap();
        let listing = build_listing(ConnectomeRoots::from_primary(&dir));
        let rec = listing.records.get(entry.id).unwrap();
        assert_eq!(date_of(&rec.accepted_at).len(), 10);
        assert!(!rec.stale);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn state_text_names_the_place() {
        let p = PathBuf::from("/c/x");
        let installed = ConnectomeState::Installed(p);
        assert!(state_text(&installed, Some(RootKind::Primary)).starts_with("Installed at"));
        assert!(state_text(&installed, Some(RootKind::External)).contains("Lead-DBS"));
        assert_eq!(
            state_text(&ConnectomeState::Available { size: 1 }, None),
            "Not installed"
        );
        assert_eq!(
            state_text(
                &ConnectomeState::Partial {
                    percent: 40,
                    bytes: 4
                },
                None
            ),
            "Partial download (40 %)"
        );
    }

    #[test]
    fn labels_and_rates_read_well() {
        assert_eq!(bytes_label(4_289_260_330), "4.3 GB");
        assert_eq!(bytes_label(685_944_962), "686 MB");
        assert_eq!(bytes_label(312_000), "312 kB");
        assert_eq!(bytes_label(1_000_000), "1 MB");
        assert_eq!(bytes_label(999_600), "1 MB");
        assert_eq!(bytes_label(499_999), "500 kB");
        assert_eq!(bytes_label(0), "0 B");
        assert_eq!(bytes_label(999), "999 B");
        assert_eq!(eta_label(30.0), "less than a minute");
        assert_eq!(eta_label(600.0), "10 min");
        assert_eq!(eta_label(7500.0), "2 h 5 min");
        assert_eq!(transfer_rate(None, 10.0, 5), None);
        assert_eq!(transfer_rate(Some((0.0, 0)), 1.0, 100), None);
        assert_eq!(
            transfer_rate(Some((0.0, 0)), 4.0, 8_000_000),
            Some(2_000_000.0)
        );
        let text = progress_text(2_000_000_000, 4_000_000_000, Some(10_000_000.0));
        assert!(text.contains("2.0 GB of 4.0 GB"));
        assert!(text.contains("10.0 MB/s"));
        assert!(text.contains("about 4 min left"));
        assert_eq!(progress_text(5_000_000, 10_000_000, None), "5 MB of 10 MB");
    }

    #[test]
    fn errors_are_explained_in_plain_language_with_their_code() {
        let busy = plain_message(&ConnectomeError::new(ErrorCode::Busy, "locked"));
        assert!(busy.starts_with("E_CONN_BUSY"));
        assert!(busy.contains("already running"));
        let host = plain_message(&ConnectomeError::new(ErrorCode::Host, "dns"));
        assert!(host.contains("cannot be reached"));
        assert!(host.contains("Details: dns"));
        let cancelled = plain_message(&ConnectomeError::cancelled());
        assert!(!cancelled.contains("E_CONN"));
        for code in [
            ErrorCode::Terms,
            ErrorCode::Disk,
            ErrorCode::Checksum,
            ErrorCode::Changed,
            ErrorCode::Size,
            ErrorCode::Archive,
            ErrorCode::Format,
            ErrorCode::Missing,
            ErrorCode::Io,
            ErrorCode::Usage,
        ] {
            let text = plain_message(&ConnectomeError::new(code, "x"));
            assert!(text.starts_with(code.code()));
        }
    }

    #[test]
    fn a_host_failure_marks_the_panel_offline_until_checked_again() {
        let mut panel = ConnectomePanel::default();
        let (tx, rx) = mpsc::channel();
        tx.send(Msg::Done(Err(ConnectomeError::new(
            ErrorCode::Host,
            "down",
        ))))
        .unwrap();
        panel.job = Some(Job {
            kind: JobKind::Fetch,
            label: String::new(),
            entry_id: None,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            cancelled: false,
            downloaded: 0,
            total: 0,
            file: None,
            phase: Phase::Downloading,
            retry: None,
            started: Instant::now(),
            first_sample: None,
        });
        panel.poll_job();
        assert!(panel.offline);
        assert!(panel.error.as_deref().unwrap().contains("E_CONN_HOST"));
        assert!(panel.job.is_none());
        let ctx = egui::Context::default();
        let mut state = DbslocAppState::default();
        panel.apply(&ctx, &mut state, Action::CheckAgain);
        assert!(!panel.offline);
    }

    #[test]
    fn a_cancelled_job_ends_quietly() {
        let mut panel = ConnectomePanel::default();
        let (tx, rx) = mpsc::channel();
        tx.send(Msg::Done(Err(ConnectomeError::cancelled())))
            .unwrap();
        panel.job = Some(Job {
            kind: JobKind::Fetch,
            label: String::new(),
            entry_id: None,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            cancelled: true,
            downloaded: 0,
            total: 0,
            file: None,
            phase: Phase::Downloading,
            retry: None,
            started: Instant::now(),
            first_sample: None,
        });
        panel.poll_job();
        assert!(panel.error.is_none());
        assert!(panel.notice.as_deref().unwrap().contains("cancelled"));
        assert!(!panel.offline);
    }

    #[test]
    fn the_window_draws_without_panicking_and_does_not_probe_the_network() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_draw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = DbslocAppState::default();
        state.stage.connectomes_dir = Some(dir.clone());
        let mut panel = ConnectomePanel::default();
        panel.open();
        let ctx = egui::Context::default();
        for _ in 0..40 {
            ctx.run_ui(egui::RawInput::default(), |ui| {
                panel.show(ui.ctx(), &mut state);
            })
            .textures_delta
            .clear();
            if panel.listing.is_some() {
                thread::sleep(std::time::Duration::from_millis(10));
            } else {
                thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        assert!(panel.listing.is_some());
        assert!(panel.job.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_filter_chips_pick_by_state_and_kind() {
        let items = [
            (true, ConnectomeClass::Dmri),
            (false, ConnectomeClass::Fmri),
            (false, ConnectomeClass::DmriMultiTract),
            (true, ConnectomeClass::Fmri),
        ];
        assert_eq!(
            split_rows(&items, ListFilter::All),
            (vec![0, 3], vec![1, 2])
        );
        assert_eq!(
            split_rows(&items, ListFilter::Installed),
            (vec![0, 3], vec![])
        );
        assert_eq!(
            split_rows(&items, ListFilter::Structural),
            (vec![0], vec![2])
        );
        assert_eq!(
            split_rows(&items, ListFilter::Functional),
            (vec![3], vec![1])
        );
        assert!(!other_visible(
            ListFilter::Installed,
            false,
            ConnectomeClass::Dmri
        ));
        assert!(other_visible(
            ListFilter::Functional,
            true,
            ConnectomeClass::Fmri
        ));
    }

    #[test]
    fn the_first_installed_row_is_selected_by_default() {
        assert_eq!(default_selection(&[]), None);
        assert_eq!(default_selection(&[false, false]), Some(0));
        assert_eq!(default_selection(&[false, true, true]), Some(1));
    }

    #[test]
    fn the_licence_section_opens_only_while_there_is_something_to_accept() {
        assert!(licence_open_by_default(false, false));
        assert!(!licence_open_by_default(false, true));
        assert!(!licence_open_by_default(true, false));
    }

    #[test]
    fn the_layout_stacks_below_the_narrow_width_and_keeps_a_readable_list() {
        let unit = 14.0;
        assert!(is_narrow(500.0, unit));
        assert!(!is_narrow(900.0, unit));
        for width in [640.0_f32, 900.0, 1400.0] {
            let w = list_width(width, unit);
            assert!(w >= unit * 18.5 && w <= width * 0.5);
        }
    }

    #[test]
    fn a_row_subtitle_names_the_kind_and_the_size_or_the_partial_progress() {
        let available = ConnectomeState::Available { size: 1 };
        assert_eq!(
            row_subtitle("Structural", &available, 4_289_260_330),
            "Structural · 4.3 GB"
        );
        let partial = ConnectomeState::Partial {
            percent: 40,
            bytes: 4,
        };
        assert_eq!(
            row_subtitle("Functional", &partial, 10),
            "Functional · partial 40 %"
        );
    }

    #[test]
    fn asking_for_an_entry_selects_it_and_opens_its_licence() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_sel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let listing = build_listing(ConnectomeRoots::from_primary(&dir));
        let target = connectome_catalog::catalogue().last().unwrap().id;
        let mut panel = ConnectomePanel::default();
        panel.settle_selection(&listing);
        assert!(panel.selected.is_some());
        panel.expand_once = Some(target.to_owned());
        panel.settle_selection(&listing);
        assert_eq!(panel.selected, Some(Selection::Entry(target)));
        assert_eq!(panel.licence_once, Some(target));
        assert!(panel.expand_once.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_window_draws_at_wide_and_narrow_sizes() {
        let dir = std::env::temp_dir().join(format!("dbsloc_conn_size_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = DbslocAppState::default();
        state.stage.connectomes_dir = Some(dir.clone());
        for size in [egui::vec2(1400.0, 900.0), egui::vec2(480.0, 700.0)] {
            let mut panel = ConnectomePanel::default();
            panel.open();
            let ctx = egui::Context::default();
            for _ in 0..30 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                ctx.run_ui(input, |ui| {
                    panel.show(ui.ctx(), &mut state);
                })
                .textures_delta
                .clear();
                thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(panel.listing.is_some());
            assert!(panel.selected.is_some());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
