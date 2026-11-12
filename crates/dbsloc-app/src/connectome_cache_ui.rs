// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use dbsloc_pipeline::connectome_cache::{
    self, CacheInfo, CacheSetting, Choice, DeclinedEntry, DeleteResult, Estimate,
};
use dbsloc_pipeline::connectome_maps::job::ResolvedConnectome;
use dbsloc_pipeline::connectome_maps::source::cache_target;
use dbsloc_pipeline::connectome_maps::{MapResult, Progress};
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::connectome_panel::bytes_label;
use crate::panels::{em, info_icon, palette};

const SECONDS_PER_10M_POINTS: f64 = 0.44;

static ASKING: AtomicBool = AtomicBool::new(false);
static NEXT_FLOW: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheMode {
    #[default]
    Ask,
    Always,
    Never,
}

impl CacheMode {
    pub const ALL: [CacheMode; 3] = [CacheMode::Ask, CacheMode::Always, CacheMode::Never];

    pub fn label(self) -> &'static str {
        match self {
            CacheMode::Ask => "Ask",
            CacheMode::Always => "Always",
            CacheMode::Never => "Never",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            CacheMode::Ask => "Ask before a connectome gets a ready-to-use copy",
            CacheMode::Always => {
                "Keep a ready-to-use copy without asking when the drive has twice its size plus 5 GB free"
            }
            CacheMode::Never => "Never keep a copy; every job reads the original files",
        }
    }

    pub fn setting(self) -> CacheSetting {
        match self {
            CacheMode::Ask => CacheSetting::Ask,
            CacheMode::Always => CacheSetting::Always,
            CacheMode::Never => CacheSetting::Never,
        }
    }
}

pub const MODE_INFO: &str = "A ready-to-use copy makes later jobs with a large connectome start \
     in well under a second. It takes about 1 GB of disk space for a connectome like Gibbs 19 and \
     can be deleted with Manage cache.";

pub fn needs_consent(setting: CacheSetting, recorded: Option<Choice>, cache_valid: bool) -> bool {
    setting == CacheSetting::Ask && recorded.is_none() && !cache_valid
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsentRequest {
    pub key: String,
    pub name: String,
    pub estimate: Estimate,
}

pub fn check_consent(conn: &ResolvedConnectome, setting: CacheSetting) -> Option<ConsentRequest> {
    if setting != CacheSetting::Ask {
        return None;
    }
    let target = cache_target(conn);
    target.source_file()?;
    let key = target.key();
    let recorded = connectome_cache::choice(&key).map(|r| r.choice);
    let valid = connectome_cache::is_valid(&target);
    if !needs_consent(setting, recorded, valid) {
        return None;
    }
    let estimate = connectome_cache::estimate(&target).ok()?;
    Some(ConsentRequest {
        key,
        name: conn.name.clone(),
        estimate,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Keep,
    NotNow { remember: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheRun {
    pub setting: CacheSetting,
    pub build_first: bool,
    pub record_decline: Option<u64>,
}

impl CacheRun {
    pub fn plain(setting: CacheSetting) -> Self {
        CacheRun {
            setting,
            build_first: false,
            record_decline: None,
        }
    }
}

pub fn run_after(answer: Answer, request: &ConsentRequest) -> CacheRun {
    match answer {
        Answer::Keep => CacheRun {
            setting: CacheSetting::Ask,
            build_first: true,
            record_decline: None,
        },
        Answer::NotNow { remember } => CacheRun {
            setting: CacheSetting::Never,
            build_first: false,
            record_decline: remember.then_some(request.estimate.estimated_bytes),
        },
    }
}

pub fn fake_request() -> ConsentRequest {
    let points = 70_354_484;
    ConsentRequest {
        key: "dMRI-Gibbs19-00000000".to_owned(),
        name: "Gibbs 19 Full".to_owned(),
        estimate: Estimate {
            points,
            estimated_bytes: (points as f64 * connectome_cache::BYTES_PER_POINT) as u64,
            free_bytes: Some(183_000_000_000),
        },
    }
}

pub fn keep_blocker(estimate: &Estimate) -> Option<String> {
    if estimate.dialog_ok() {
        return None;
    }
    let free = estimate.free_bytes.unwrap_or(0);
    let left = free.saturating_sub(estimate.estimated_bytes);
    Some(format!(
        "Only {} would stay free after the copy; at least 5 GB must stay free",
        bytes_label(left)
    ))
}

pub fn plain_load_seconds(points: u64) -> u64 {
    ((points as f64 / 10_000_000.0) * SECONDS_PER_10M_POINTS)
        .ceil()
        .max(1.0) as u64
}

pub fn consent_text(request: &ConsentRequest) -> String {
    let e = &request.estimate;
    let free = e
        .free_bytes
        .map(|f| format!(" ({} free)", bytes_label(f)))
        .unwrap_or_default();
    format!(
        "Keep a ready-to-use copy on this computer: about {} of extra disk space{free}. Later jobs with this connectome then start in well under a second instead of about {} s.",
        bytes_label(e.estimated_bytes),
        plain_load_seconds(e.points)
    )
}

pub struct ConsentDialog {
    pub request: ConsentRequest,
    pub remember: bool,
    id: u64,
}

impl ConsentDialog {
    pub fn new(request: ConsentRequest) -> Self {
        Self::with_id(request, 0)
    }

    fn with_id(request: ConsentRequest, id: u64) -> Self {
        ConsentDialog {
            request,
            remember: true,
            id,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        let blocker = keep_blocker(&self.request.estimate);
        egui::Window::new(format!("Speed up {}?", self.request.name))
            .id(egui::Id::new(("connectome_cache_consent", self.id)))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let unit = em(ui);
                ui.set_max_width(unit * 22.0);
                ui.add(egui::Label::new(consent_text(&self.request)).wrap());
                if let Some(reason) = &blocker {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(reason)
                                .small()
                                .color(palette(ui).warn),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(unit * 0.3);
                ui.checkbox(&mut self.remember, "Don't ask again for this connectome")
                    .on_hover_text(
                        "Remember Not now for this connectome; Manage cache lists it so you can be asked again",
                    );
                ui.add_space(unit * 0.3);
                ui.horizontal(|ui| {
                    let keep = ui
                        .add_enabled(blocker.is_none(), egui::Button::new("Keep a copy"))
                        .on_hover_text(blocker.clone().unwrap_or_else(|| {
                            "Build the copy now as part of this job; later jobs reuse it".to_owned()
                        }));
                    if keep.clicked() {
                        answer = Some(Answer::Keep);
                    }
                    if ui
                        .button("Not now")
                        .on_hover_text("Run this job with the original files")
                        .clicked()
                    {
                        answer = Some(Answer::NotNow {
                            remember: self.remember,
                        });
                    }
                });
            });
        if answer.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            answer = Some(Answer::NotNow { remember: false });
        }
        answer
    }
}

type ProbeRx = mpsc::Receiver<Option<ConsentRequest>>;

enum FlowState {
    Idle,
    Immediate(CacheSetting),
    Probing(CacheSetting, ProbeRx),
    Waiting(Box<ConsentRequest>),
    Asking(Box<ConsentDialog>),
}

pub struct ConsentFlow {
    state: FlowState,
    id: u64,
}

impl Default for ConsentFlow {
    fn default() -> Self {
        ConsentFlow {
            state: FlowState::Idle,
            id: NEXT_FLOW.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl Drop for ConsentFlow {
    fn drop(&mut self) {
        if matches!(self.state, FlowState::Asking(_)) {
            ASKING.store(false, Ordering::Release);
        }
    }
}

fn try_take_asking() -> bool {
    ASKING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

impl ConsentFlow {
    pub fn is_active(&self) -> bool {
        !matches!(self.state, FlowState::Idle)
    }

    #[cfg(test)]
    fn is_waiting_for_other(&self) -> bool {
        matches!(self.state, FlowState::Waiting(_))
    }

    pub fn begin(
        &mut self,
        ctx: &egui::Context,
        setting: CacheSetting,
        probe: impl FnOnce() -> Option<ConsentRequest> + Send + 'static,
    ) {
        if self.is_active() {
            return;
        }
        if setting != CacheSetting::Ask {
            self.state = FlowState::Immediate(setting);
            ctx.request_repaint();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(probe());
            ctx.request_repaint();
        });
        self.state = FlowState::Probing(setting, rx);
    }

    fn start_asking(&mut self, request: ConsentRequest, ctx: &egui::Context) {
        if try_take_asking() {
            self.state = FlowState::Asking(Box::new(ConsentDialog::with_id(request, self.id)));
        } else {
            self.state = FlowState::Waiting(Box::new(request));
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        ctx.request_repaint();
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<CacheRun> {
        match &mut self.state {
            FlowState::Idle => None,
            FlowState::Immediate(setting) => {
                let run = CacheRun::plain(*setting);
                self.state = FlowState::Idle;
                Some(run)
            }
            FlowState::Probing(setting, rx) => {
                let setting = *setting;
                match rx.try_recv() {
                    Ok(Some(request)) => {
                        self.start_asking(request, ctx);
                        None
                    }
                    Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                        self.state = FlowState::Idle;
                        Some(CacheRun::plain(setting))
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(Duration::from_millis(100));
                        None
                    }
                }
            }
            FlowState::Waiting(request) => {
                let request = (**request).clone();
                self.start_asking(request, ctx);
                None
            }
            FlowState::Asking(dialog) => {
                let answer = dialog.show(ctx)?;
                let run = run_after(answer, &dialog.request);
                self.state = FlowState::Idle;
                ASKING.store(false, Ordering::Release);
                Some(run)
            }
        }
    }
}

pub struct Prepared {
    pub setting: CacheSetting,
    pub warnings: Vec<String>,
}

pub fn pre_load(
    conn: &ResolvedConnectome,
    run: &CacheRun,
    cancel: &AtomicBool,
    progress: &(dyn Fn(&Progress) + Sync),
) -> MapResult<Prepared> {
    let target = cache_target(conn);
    if let Some(bytes) = run.record_decline {
        let _ = connectome_cache::set_choice(&target.key(), &target.name, Choice::Decline, bytes);
    }
    let mut prepared = Prepared {
        setting: run.setting,
        warnings: Vec::new(),
    };
    if !run.build_first {
        return Ok(prepared);
    }
    let relabel = |p: &Progress| {
        progress(&Progress {
            stage: "Building a ready-to-use copy".to_owned(),
            done: p.done,
            total: p.total,
            message: if p.message.is_empty() {
                target.name.clone()
            } else {
                p.message.clone()
            },
        });
    };
    relabel(&Progress {
        stage: String::new(),
        done: 0,
        total: 0,
        message: String::new(),
    });
    match connectome_cache::build_and_allow(&target, Some(cancel), Some(&relabel)) {
        Ok(_) => {}
        Err(dbsloc_pipeline::connectome_maps::MapError::Cancelled) => {
            return Err(dbsloc_pipeline::connectome_maps::MapError::Cancelled)
        }
        Err(e) => {
            prepared.setting = CacheSetting::Never;
            prepared.warnings.push(format!(
                "connectome cache for {} not built: {e}",
                target.name
            ));
        }
    }
    Ok(prepared)
}

#[derive(Debug, Clone)]
pub struct CacheRow {
    pub info: CacheInfo,
    pub ticked: bool,
}

pub fn ticked_total(rows: &[CacheRow]) -> (usize, u64) {
    rows.iter()
        .filter(|r| r.ticked)
        .fold((0, 0), |(n, b), r| (n + 1, b + r.info.size_bytes))
}

pub fn set_all(rows: &mut [CacheRow], on: bool) {
    rows.iter_mut().for_each(|r| r.ticked = on);
}

pub fn ticked_keys(rows: &[CacheRow]) -> Vec<String> {
    rows.iter()
        .filter(|r| r.ticked)
        .map(|r| r.info.key.clone())
        .collect()
}

pub fn confirm_text(count: usize, bytes: u64) -> String {
    format!(
        "Delete {count} ready-to-use cop{} ({})?",
        if count == 1 { "y" } else { "ies" },
        bytes_label(bytes)
    )
}

pub fn ago_text(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let days = secs / 86_400;
    if days >= 2 {
        format!("{days} days ago")
    } else if secs >= 3600 {
        format!("{} h ago", secs / 3600)
    } else if secs >= 60 {
        format!("{} min ago", secs / 60)
    } else {
        "just now".to_owned()
    }
}

pub fn last_used_text(last: Option<SystemTime>, now: SystemTime) -> String {
    match last {
        Some(t) => ago_text(now.duration_since(t).unwrap_or_default()),
        None => "never".to_owned(),
    }
}

pub fn report_lines(results: &[DeleteResult], rows: &[CacheRow]) -> Vec<String> {
    let name = |key: &str| {
        rows.iter()
            .find(|r| r.info.key == key)
            .map_or_else(|| key.to_owned(), |r| r.info.name.clone())
    };
    results
        .iter()
        .filter_map(|r| match r {
            DeleteResult::Deleted { .. } => None,
            DeleteResult::InUse { key } => Some(format!(
                "{} is in use by a running job and was kept",
                name(key)
            )),
            DeleteResult::Failed { key, message } => {
                Some(format!("{} could not be deleted: {message}", name(key)))
            }
        })
        .collect()
}

struct Loaded {
    rows: Vec<CacheInfo>,
    declined: Vec<DeclinedEntry>,
    report: Vec<String>,
}

fn list_all() -> (Vec<CacheInfo>, Vec<DeclinedEntry>) {
    (
        connectome_cache::list_caches(),
        connectome_cache::declined(),
    )
}

#[derive(Default)]
pub struct CacheManager {
    open: bool,
    started: bool,
    rows: Vec<CacheRow>,
    declined: Vec<DeclinedEntry>,
    report: Vec<String>,
    rx: Option<mpsc::Receiver<Loaded>>,
    confirm: bool,
}

impl CacheManager {
    pub fn open(&mut self) {
        self.open = true;
        self.started = false;
        self.report.clear();
        self.confirm = false;
    }

    fn spawn(&mut self, ctx: &egui::Context, task: impl FnOnce() -> Vec<String> + Send + 'static) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let report = task();
            let (rows, declined) = list_all();
            let _ = tx.send(Loaded {
                rows,
                declined,
                report,
            });
            ctx.request_repaint();
        });
    }

    fn apply_loaded(&mut self, loaded: Loaded) {
        let ticked: Vec<String> = ticked_keys(&self.rows);
        self.rows = loaded
            .rows
            .into_iter()
            .map(|info| CacheRow {
                ticked: ticked.contains(&info.key),
                info,
            })
            .collect();
        self.declined = loaded.declined;
        if !loaded.report.is_empty() {
            self.report = loaded.report;
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(loaded) => {
                    self.apply_loaded(loaded);
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.rx = None,
            }
        }
        if self.open && !self.started {
            self.started = true;
            self.spawn(ctx, Vec::new);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Connectome cache")
            .id(egui::Id::new("connectome_cache_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width((ctx.content_rect().width() * 0.5).clamp(360.0, 640.0))
            .max_size(ctx.content_rect().size() * 0.95)
            .show(ctx, |ui| self.contents(ui, ctx));
        if !open {
            self.open = false;
            self.confirm = false;
        }
        if self.confirm {
            self.confirm_window(ctx);
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let pal = palette(ui);
        let unit = em(ui);
        let busy = self.rx.is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.rows.is_empty(), egui::Button::new("Select all"))
                .on_hover_text("Tick every copy")
                .clicked()
            {
                set_all(&mut self.rows, true);
            }
            if ui
                .add_enabled(!self.rows.is_empty(), egui::Button::new("Select none"))
                .on_hover_text("Untick every copy")
                .clicked()
            {
                set_all(&mut self.rows, false);
            }
            let (count, bytes) = ticked_total(&self.rows);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{count} ticked · {}", bytes_label(bytes)))
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            if busy {
                ui.add(egui::Spinner::new().size(unit));
            }
        });
        egui::ScrollArea::vertical()
            .max_height(unit * 16.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.rows.is_empty() {
                    let text = if self.started && !busy {
                        "No ready-to-use copy is kept"
                    } else {
                        "Looking for copies…"
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                            .selectable(false),
                    );
                }
                let now = SystemTime::now();
                egui::Grid::new("connectome_cache_rows")
                    .num_columns(4)
                    .spacing([unit * 0.8, unit * 0.3])
                    .show(ui, |ui| {
                        for row in &mut self.rows {
                            ui.checkbox(&mut row.ticked, "")
                                .on_hover_text("Tick to delete this copy");
                            let name = ui.add(
                                egui::Label::new(egui::RichText::new(&row.info.name).strong())
                                    .selectable(false),
                            );
                            name.on_hover_text(format!("Source: {}", row.info.source_path));
                            ui.add(
                                egui::Label::new(bytes_label(row.info.size_bytes))
                                    .selectable(false),
                            );
                            let mut note =
                                format!("used {}", last_used_text(row.info.last_used, now));
                            if row.info.orphaned {
                                note.push_str(" · source missing");
                            }
                            if row.info.in_use {
                                note.push_str(" · in use");
                            }
                            let colour = if row.info.orphaned {
                                pal.warn
                            } else {
                                pal.text_muted
                            };
                            ui.add(
                                egui::Label::new(egui::RichText::new(note).color(colour))
                                    .selectable(false),
                            )
                            .on_hover_text(if row.info.orphaned {
                                "The original files are gone; this copy can no longer be used"
                            } else {
                                "When a job last used this copy"
                            });
                            ui.end_row();
                        }
                    });
            });
        let (count, _) = ticked_total(&self.rows);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(count > 0 && !busy, egui::Button::new("Delete selected…"))
                .on_hover_text("Delete the ticked copies; the connectomes themselves stay")
                .clicked()
            {
                self.confirm = true;
            }
        });
        for line in &self.report {
            ui.add(egui::Label::new(egui::RichText::new(line).small().color(pal.warn)).wrap());
        }
        if !self.declined.is_empty() {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Declined").strong());
                info_icon(
                    ui,
                    "Connectomes for which you chose not to keep a copy. Ask again lets the next job offer it once more.",
                );
            });
            let declined = self.declined.clone();
            for d in declined {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(&d.name).selectable(false))
                        .on_hover_text(format!(
                            "About {} of disk space; declined {}",
                            bytes_label(d.estimated_bytes),
                            d.decided_at
                        ));
                    if ui
                        .add_enabled(!busy, egui::Button::new("Ask again").small())
                        .on_hover_text("Offer a ready-to-use copy again at the next job")
                        .clicked()
                    {
                        let key = d.key.clone();
                        self.spawn(ctx, move || {
                            let _ = connectome_cache::reset_choice(&key);
                            Vec::new()
                        });
                    }
                });
            }
        }
    }

    fn confirm_window(&mut self, ctx: &egui::Context) {
        let (count, bytes) = ticked_total(&self.rows);
        if count == 0 {
            self.confirm = false;
            return;
        }
        let mut delete = false;
        let mut cancel = false;
        egui::Window::new("Delete copies?")
            .id(egui::Id::new("connectome_cache_confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(confirm_text(count, bytes));
                ui.horizontal(|ui| {
                    if ui
                        .button("Delete")
                        .on_hover_text(
                            "Delete the copies; the next job with them reads the original files",
                        )
                        .clicked()
                    {
                        delete = true;
                    }
                    if ui.button("Keep").on_hover_text("Keep the copies").clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cancel = true;
        }
        if delete {
            self.confirm = false;
            let keys = ticked_keys(&self.rows);
            let rows = self.rows.clone();
            self.spawn(ctx, move || {
                let results = connectome_cache::delete_caches(&keys);
                report_lines(&results, &rows)
            });
        } else if cancel {
            self.confirm = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate(points: u64, free: Option<u64>) -> Estimate {
        Estimate {
            points,
            estimated_bytes: (points as f64 * 14.4) as u64,
            free_bytes: free,
        }
    }

    fn request() -> ConsentRequest {
        ConsentRequest {
            key: "dMRI-Gibbs-12345678".to_owned(),
            name: "Gibbs 19".to_owned(),
            estimate: estimate(70_354_484, Some(200_000_000_000)),
        }
    }

    fn info(key: &str, size: u64) -> CacheInfo {
        CacheInfo {
            key: key.to_owned(),
            name: key.to_owned(),
            source_path: String::new(),
            size_bytes: size,
            last_used: None,
            orphaned: false,
            in_use: false,
        }
    }

    #[test]
    fn the_dialog_shows_only_under_ask_without_a_choice_or_a_valid_cache() {
        assert!(needs_consent(CacheSetting::Ask, None, false));
        assert!(!needs_consent(
            CacheSetting::Ask,
            Some(Choice::Allow),
            false
        ));
        assert!(!needs_consent(
            CacheSetting::Ask,
            Some(Choice::Decline),
            false
        ));
        assert!(!needs_consent(CacheSetting::Ask, None, true));
        assert!(!needs_consent(CacheSetting::Always, None, false));
        assert!(!needs_consent(CacheSetting::Never, None, false));
    }

    #[test]
    fn keep_a_copy_builds_first_and_records_nothing_itself() {
        let run = run_after(Answer::Keep, &request());
        assert!(run.build_first);
        assert_eq!(run.setting, CacheSetting::Ask);
        assert_eq!(run.record_decline, None);
    }

    #[test]
    fn not_now_uses_the_plain_reader_and_records_a_decline_only_when_ticked() {
        let r = request();
        let remembered = run_after(Answer::NotNow { remember: true }, &r);
        assert!(!remembered.build_first);
        assert_eq!(remembered.setting, CacheSetting::Never);
        assert_eq!(remembered.record_decline, Some(r.estimate.estimated_bytes));
        let once = run_after(Answer::NotNow { remember: false }, &r);
        assert_eq!(once.record_decline, None);
        assert_eq!(once.setting, CacheSetting::Never);
    }

    #[test]
    fn a_plain_run_keeps_the_setting() {
        let run = CacheRun::plain(CacheSetting::Always);
        assert_eq!(run.setting, CacheSetting::Always);
        assert!(!run.build_first);
        assert_eq!(run.record_decline, None);
    }

    #[test]
    fn keep_is_blocked_below_five_gigabytes_after_the_build() {
        assert_eq!(keep_blocker(&estimate(70_354_484, None)), None);
        assert_eq!(
            keep_blocker(&estimate(70_354_484, Some(200_000_000_000))),
            None
        );
        let reason = keep_blocker(&estimate(70_354_484, Some(5_500_000_000)));
        assert!(reason.is_some_and(|r| r.contains("5 GB")));
    }

    #[test]
    fn the_text_names_size_free_space_and_load_time() {
        let text = consent_text(&request());
        assert!(text.contains("1.0 GB"));
        assert!(text.contains("200.0 GB free"));
        assert!(text.contains("about 4 s"));
        assert_eq!(plain_load_seconds(1), 1);
    }

    #[test]
    fn the_text_omits_free_space_when_unknown() {
        let mut r = request();
        r.estimate.free_bytes = None;
        assert!(!consent_text(&r).contains("free)"));
    }

    #[test]
    fn modes_map_to_settings_and_old_states_default_to_ask() {
        assert_eq!(CacheMode::default(), CacheMode::Ask);
        assert_eq!(CacheMode::Always.setting(), CacheSetting::Always);
        assert_eq!(CacheMode::Never.setting(), CacheSetting::Never);
        let parsed: CacheMode = serde_json::from_str("\"Never\"").unwrap_or_default();
        assert_eq!(parsed, CacheMode::Never);
    }

    #[test]
    fn ticked_rows_are_counted_and_summed() {
        let mut rows = vec![
            CacheRow {
                info: info("a", 1_000_000_000),
                ticked: false,
            },
            CacheRow {
                info: info("b", 2_500_000_000),
                ticked: false,
            },
        ];
        assert_eq!(ticked_total(&rows), (0, 0));
        rows[1].ticked = true;
        assert_eq!(ticked_total(&rows), (1, 2_500_000_000));
        assert_eq!(ticked_keys(&rows), vec!["b".to_owned()]);
        set_all(&mut rows, true);
        assert_eq!(ticked_total(&rows), (2, 3_500_000_000));
        set_all(&mut rows, false);
        assert_eq!(ticked_total(&rows), (0, 0));
    }

    #[test]
    fn the_confirmation_names_count_and_size() {
        assert_eq!(
            confirm_text(1, 1_040_000_000),
            "Delete 1 ready-to-use copy (1.0 GB)?"
        );
        assert_eq!(
            confirm_text(3, 6_700_000_000),
            "Delete 3 ready-to-use copies (6.7 GB)?"
        );
    }

    #[test]
    fn in_use_and_failed_rows_are_reported_and_deleted_ones_are_not() {
        let rows = vec![CacheRow {
            info: info("a", 1),
            ticked: true,
        }];
        let results = vec![
            DeleteResult::Deleted {
                key: "z".to_owned(),
                freed_bytes: 5,
            },
            DeleteResult::InUse {
                key: "a".to_owned(),
            },
            DeleteResult::Failed {
                key: "q".to_owned(),
                message: "denied".to_owned(),
            },
        ];
        let lines = report_lines(&results, &rows);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("in use"));
        assert!(lines[1].contains("denied"));
    }

    #[test]
    fn last_used_reads_in_words() {
        let now = SystemTime::now();
        assert_eq!(last_used_text(None, now), "never");
        assert_eq!(last_used_text(Some(now), now), "just now");
        assert_eq!(
            last_used_text(Some(now - Duration::from_secs(3 * 86_400)), now),
            "3 days ago"
        );
        assert_eq!(ago_text(Duration::from_secs(7200)), "2 h ago");
    }

    fn step(flow: &mut ConsentFlow, ctx: &egui::Context) -> Option<CacheRun> {
        let mut run = None;
        ctx.run_ui(egui::RawInput::default(), |ui| run = flow.poll(ui.ctx()))
            .textures_delta
            .clear();
        run
    }

    #[test]
    fn a_fixed_setting_starts_without_a_dialog() {
        for setting in [CacheSetting::Never, CacheSetting::Always] {
            let ctx = egui::Context::default();
            let mut flow = ConsentFlow::default();
            flow.begin(&ctx, setting, || panic!("must not probe"));
            assert!(flow.is_active());
            assert_eq!(step(&mut flow, &ctx), Some(CacheRun::plain(setting)));
            assert!(!flow.is_active());
        }
    }

    #[test]
    fn only_one_flow_asks_at_a_time() {
        let ctx = egui::Context::default();
        let mut a = ConsentFlow::default();
        let mut b = ConsentFlow::default();
        assert_ne!(a.id, b.id);
        a.begin(&ctx, CacheSetting::Ask, || Some(request()));
        b.begin(&ctx, CacheSetting::Ask, || Some(request()));
        for _ in 0..300 {
            let _ = step(&mut a, &ctx);
            let _ = step(&mut b, &ctx);
            if matches!(a.state, FlowState::Asking(_)) && b.is_waiting_for_other() {
                break;
            }
            if matches!(b.state, FlowState::Asking(_)) && a.is_waiting_for_other() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let asking = |f: &ConsentFlow| matches!(f.state, FlowState::Asking(_));
        assert!(asking(&a) != asking(&b));
        assert!(a.is_waiting_for_other() || b.is_waiting_for_other());
        let (first, mut second) = if asking(&a) { (a, b) } else { (b, a) };
        drop(first);
        let _ = step(&mut second, &ctx);
        assert!(asking(&second));
        drop(second);
        assert!(!ASKING.load(Ordering::Acquire));
    }

    #[test]
    fn the_consent_dialog_defaults_to_remembering() {
        assert!(ConsentDialog::new(request()).remember);
    }
}
