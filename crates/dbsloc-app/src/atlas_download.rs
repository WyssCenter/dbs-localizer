// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;

use dbsloc_pipeline::atlas_fetch::{
    self, AtlasErrorKind, AtlasEvent, AtlasFetchError, AtlasFetchSettings, AtlasState, AtlasStatus,
    DOWNLOAD_NOTICE,
};
use dbsloc_pipeline::template_catalog;
use eframe::egui;

use crate::panels::{em, humanize_message, palette};

pub const WINDOW_TITLE: &str = "Download atlases";
pub const INSTALLED_NOTE: &str =
    "Installed — shown in the Atlas set list on the subject's next Reconstruct or Visualize run.";

pub fn no_data_dir_message() -> String {
    format!(
        "No per-user data folder is available, so atlases cannot be downloaded. Set the {} \
         environment variable to a folder you can write to and start Wyss DBS Localizer again.",
        template_catalog::DATA_DIR_ENV
    )
}

pub fn atlas_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

fn sentence(text: &str) -> String {
    let text = humanize_message(text.trim());
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return text;
    };
    let mut out: String = first.to_uppercase().chain(chars).collect();
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

pub fn error_text(e: &AtlasFetchError) -> String {
    if e.kind == AtlasErrorKind::Busy {
        format!(
            "Another atlas download or removal is running on this computer; try again when it has finished. Details: {}",
            sentence(&e.message)
        )
    } else {
        sentence(&e.message)
    }
}

pub fn progress_text(name: &str, p: &Progress) -> (f32, String) {
    if p.installing {
        return (1.0, format!("{name}: installing…"));
    }
    let total = p.total.max(p.downloaded);
    let frac = if total > 0 {
        (p.downloaded as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (
        frac,
        format!(
            "{name}: {} of {} downloaded",
            atlas_size(p.downloaded),
            atlas_size(total)
        ),
    )
}

pub struct Listing {
    pub rows: Result<Vec<AtlasStatus>, String>,
    pub data_root: Option<PathBuf>,
}

pub type Lister = Box<dyn Fn() -> Listing + Send>;
type RemoveResult = Result<PathBuf, AtlasFetchError>;

fn default_listing() -> Listing {
    Listing {
        rows: atlas_fetch::list_atlases().map_err(|e| e.to_string()),
        data_root: template_catalog::user_data_dir(),
    }
}

enum Msg {
    Event(AtlasEvent),
    Finished(Result<PathBuf, AtlasFetchError>),
}

struct Active {
    name: String,
    rx: mpsc::Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    cancelled: bool,
    downloaded: u64,
    total: u64,
    installing: bool,
    note: Option<String>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

struct Removing {
    name: String,
    rx: mpsc::Receiver<RemoveResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    Nothing,
    Download { enabled: bool },
    Cancel,
    Remove { enabled: bool },
    NotRemovable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    pub installing: bool,
}

enum Act {
    Download(String),
    Cancel,
    Remove(String),
}

#[derive(Default)]
pub struct AtlasDownloadWindow {
    open: bool,
    filter: String,
    rows: Vec<AtlasStatus>,
    list_error: Option<String>,
    data_root: Option<PathBuf>,
    active: Option<Active>,
    removing: Option<Removing>,
    errors: BTreeMap<String, String>,
    installed: BTreeSet<String>,
    lister: Option<Lister>,
}

impl AtlasDownloadWindow {
    #[cfg(test)]
    pub fn with_lister(lister: Lister) -> Self {
        Self {
            lister: Some(lister),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
        self.refresh();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn refresh(&mut self) {
        let listing = match &self.lister {
            Some(lister) => lister(),
            None => default_listing(),
        };
        match listing.rows {
            Ok(rows) => {
                self.rows = rows;
                self.list_error = None;
            }
            Err(e) => {
                self.rows.clear();
                self.list_error = Some(sentence(&e));
            }
        }
        self.data_root = listing.data_root;
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[AtlasStatus] {
        &self.rows
    }

    #[cfg(test)]
    pub fn set_filter(&mut self, filter: &str) {
        filter.clone_into(&mut self.filter);
    }

    pub fn visible_rows(&self) -> Vec<&AtlasStatus> {
        let needle = self.filter.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn downloading(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.name.as_str())
    }

    pub fn removing(&self) -> Option<&str> {
        self.removing.as_ref().map(|r| r.name.as_str())
    }

    #[cfg(test)]
    pub fn has_data_root(&self) -> bool {
        self.data_root.is_some()
    }

    pub fn error_for(&self, name: &str) -> Option<&str> {
        self.errors.get(name).map(String::as_str)
    }

    pub fn is_freshly_installed(&self, name: &str) -> bool {
        self.installed.contains(name)
    }

    pub fn progress(&self) -> Option<Progress> {
        self.active.as_ref().map(|a| Progress {
            downloaded: a.downloaded,
            total: a.total,
            installing: a.installing,
        })
    }

    pub fn note(&self) -> Option<&str> {
        self.active.as_ref().and_then(|a| a.note.as_deref())
    }

    fn can_download(&self) -> bool {
        self.active.is_none()
            && self.removing.is_none()
            && self.data_root.is_some()
            && self.list_error.is_none()
    }

    fn can_remove(&self) -> bool {
        self.active.is_none() && self.removing.is_none() && self.data_root.is_some()
    }

    fn download_hover(&self, row: &AtlasStatus) -> String {
        if self.data_root.is_none() {
            no_data_dir_message()
        } else if self.active.is_some() {
            "Another atlas is downloading; one download runs at a time".to_owned()
        } else if self.removing.is_some() {
            "An atlas is being removed; wait until the removal has finished".to_owned()
        } else if let Some(e) = &self.list_error {
            e.clone()
        } else {
            format!(
                "Download {} ({}) to this computer",
                row.name,
                atlas_size(row.bytes)
            )
        }
    }

    fn remove_hover(&self) -> String {
        if self.data_root.is_none() {
            no_data_dir_message()
        } else if self.active.is_some() {
            "An atlas is downloading; wait until the download has finished".to_owned()
        } else if self.removing.is_some() {
            "Another atlas is being removed; wait until the removal has finished".to_owned()
        } else {
            "Delete this downloaded atlas from this computer".to_owned()
        }
    }

    pub fn row_action(&self, row: &AtlasStatus) -> RowAction {
        if self.downloading() == Some(row.name.as_str()) {
            return RowAction::Cancel;
        }
        match &row.state {
            AtlasState::Bundled(_) => RowAction::Nothing,
            state @ AtlasState::Installed { .. } if state.removable() => RowAction::Remove {
                enabled: self.can_remove(),
            },
            AtlasState::Installed { .. } => RowAction::NotRemovable,
            AtlasState::Available => RowAction::Download {
                enabled: self.can_download(),
            },
        }
    }

    pub fn start_download(&mut self, name: &str, ctx: &egui::Context) {
        if self.active.is_some() || self.removing.is_some() {
            return;
        }
        let Some(root) = self.data_root.clone() else {
            self.errors.insert(name.to_owned(), no_data_dir_message());
            return;
        };
        let owned = name.to_owned();
        self.spawn(
            name,
            move |cancel, on_event| {
                atlas_fetch::fetch_atlas(
                    &owned,
                    &root,
                    &AtlasFetchSettings::default(),
                    cancel,
                    on_event,
                )
            },
            Some(ctx.clone()),
        );
    }

    pub fn spawn<F>(&mut self, name: &str, job: F, ctx: Option<egui::Context>)
    where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(AtlasEvent)) -> Result<PathBuf, AtlasFetchError>
            + Send
            + 'static,
    {
        if self.active.is_some() || self.removing.is_some() {
            return;
        }
        self.errors.remove(name);
        self.installed.remove(name);
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let tx_events = tx.clone();
            let ctx_events = ctx.clone();
            let result = job(worker_cancel.as_ref(), &mut |ev| {
                let _ = tx_events.send(Msg::Event(ev));
                if let Some(c) = &ctx_events {
                    c.request_repaint();
                }
            });
            let _ = tx.send(Msg::Finished(result));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.active = Some(Active {
            name: name.to_owned(),
            rx,
            cancel,
            cancelled: false,
            downloaded: 0,
            total: 0,
            installing: false,
            note: None,
        });
    }

    pub fn cancel(&mut self) {
        if let Some(a) = self.active.as_mut() {
            a.cancelled = true;
            a.cancel.store(true, Ordering::SeqCst);
        }
    }

    pub fn start_remove(&mut self, name: &str, ctx: &egui::Context) {
        if self.removing.is_some() || self.active.is_some() {
            return;
        }
        let Some(root) = self.data_root.clone() else {
            self.errors.insert(name.to_owned(), no_data_dir_message());
            return;
        };
        let owned = name.to_owned();
        self.spawn_remove(
            name,
            move || {
                atlas_fetch::remove_atlas(
                    &owned,
                    &root,
                    atlas_fetch::default_bundled_dir().as_deref(),
                )
            },
            Some(ctx.clone()),
        );
    }

    pub fn spawn_remove<F>(&mut self, name: &str, job: F, ctx: Option<egui::Context>)
    where
        F: FnOnce() -> RemoveResult + Send + 'static,
    {
        if self.removing.is_some() || self.active.is_some() {
            return;
        }
        self.errors.remove(name);
        self.installed.remove(name);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(job());
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.removing = Some(Removing {
            name: name.to_owned(),
            rx,
        });
    }

    pub fn poll(&mut self) {
        let mut changed = self.poll_download();
        changed |= self.poll_remove();
        if changed {
            self.refresh();
        }
    }

    fn poll_download(&mut self) -> bool {
        let Some(a) = self.active.as_mut() else {
            return false;
        };
        let mut finished = None;
        loop {
            match a.rx.try_recv() {
                Ok(Msg::Event(AtlasEvent::Started { total_bytes, .. })) => {
                    a.total = total_bytes;
                }
                Ok(Msg::Event(AtlasEvent::Progress { downloaded, total })) => {
                    a.downloaded = downloaded;
                    a.total = total;
                }
                Ok(Msg::Event(AtlasEvent::Note { message })) => {
                    a.note = Some(sentence(&message));
                }
                Ok(Msg::Event(AtlasEvent::Retry {
                    attempt,
                    wait_secs,
                    reason,
                })) => {
                    a.note = Some(format!(
                        "Download paused ({}); retry {attempt} in {wait_secs} seconds.",
                        humanize_message(reason.trim())
                    ));
                }
                Ok(Msg::Event(AtlasEvent::Installing)) => a.installing = true,
                Ok(Msg::Event(AtlasEvent::Done { .. })) => {}
                Ok(Msg::Finished(r)) => {
                    finished = Some(r);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = Some(Err(AtlasFetchError::new(
                        AtlasErrorKind::Io,
                        "the download stopped unexpectedly",
                    )));
                    break;
                }
            }
        }
        let Some(result) = finished else {
            return false;
        };
        let Some(active) = self.active.take() else {
            return false;
        };
        let name = active.name.clone();
        let cancelled = active.cancelled;
        drop(active);
        match result {
            Ok(_) => {
                self.installed.insert(name);
            }
            Err(e) if e.is_cancelled() || cancelled => {}
            Err(e) => {
                self.errors.insert(name, error_text(&e));
            }
        }
        true
    }

    fn poll_remove(&mut self) -> bool {
        let Some(r) = self.removing.as_ref() else {
            return false;
        };
        let result = match r.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err(AtlasFetchError::new(
                AtlasErrorKind::Io,
                "the removal stopped unexpectedly",
            )),
        };
        let Some(r) = self.removing.take() else {
            return false;
        };
        if let Err(e) = result {
            self.errors.insert(r.name, error_text(&e));
        }
        true
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll();
        if !self.open {
            return;
        }
        let screen = ctx.content_rect().size();
        let unit = ctx.global_style().spacing.interact_size.y;
        let mut open = true;
        let mut acts = Vec::new();
        egui::Window::new(WINDOW_TITLE)
            .id(egui::Id::new("atlas_download_window"))
            .collapsible(false)
            .resizable(true)
            .default_width((unit * 34.0).min(screen.x * 0.92))
            .max_size(screen * 0.95)
            .open(&mut open)
            .show(ctx, |ui| self.contents(ui, screen.y, &mut acts));
        if !open {
            self.close();
        }
        for act in acts {
            match act {
                Act::Download(name) => self.start_download(&name, ctx),
                Act::Cancel => self.cancel(),
                Act::Remove(name) => self.start_remove(&name, ctx),
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, screen_h: f32, acts: &mut Vec<Act>) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.add(egui::Label::new(DOWNLOAD_NOTICE).wrap());
        ui.add_space(unit * 0.4);
        match &self.data_root {
            Some(root) => {
                let dir = atlas_fetch::downloaded_atlases_dir(root);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "Downloaded atlases are saved in {}",
                            humanize_message(&dir.display().to_string())
                        ))
                        .small()
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            None => {
                ui.add(
                    egui::Label::new(egui::RichText::new(no_data_dir_message()).color(pal.warn))
                        .wrap(),
                );
            }
        }
        if let Some(e) = &self.list_error {
            ui.add(egui::Label::new(egui::RichText::new(e).color(pal.err)).wrap());
        }
        ui.add_space(unit * 0.4);
        ui.horizontal(|ui| {
            ui.label("Filter:");
            let room = (ui.available_width() - unit * 8.0).max(unit * 6.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Part of an atlas name")
                    .desired_width(room),
            )
            .on_hover_text("Show only the atlases whose name contains this text");
            if !self.filter.is_empty()
                && ui
                    .button("Clear")
                    .on_hover_text("Show every atlas again")
                    .clicked()
            {
                self.filter.clear();
            }
        });
        let visible = self.visible_rows().len();
        ui.label(
            egui::RichText::new(format!("{visible} of {} atlases", self.rows.len()))
                .small()
                .color(pal.text_muted),
        );
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height((screen_h * 0.55).max(unit * 6.0))
            .show(ui, |ui| {
                if self.rows.is_empty() && self.list_error.is_none() {
                    ui.label(egui::RichText::new("No atlases are listed.").color(pal.text_muted));
                }
                let rows: Vec<AtlasStatus> = self.visible_rows().into_iter().cloned().collect();
                for row in &rows {
                    self.row(ui, row, acts);
                    ui.add_space(unit * 0.2);
                }
            });
    }

    fn row(&self, ui: &mut egui::Ui, row: &AtlasStatus, acts: &mut Vec<Act>) {
        let pal = palette(ui);
        let unit = em(ui);
        let h = ui.spacing().interact_size.y;
        let action = self.row_action(row);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), h),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let button = [unit * 5.5, h];
                match action {
                    RowAction::Download { enabled } => {
                        let hover = self.download_hover(row);
                        let clicked = ui
                            .add_enabled_ui(enabled, |ui| {
                                ui.add_sized(button, egui::Button::new("Download"))
                            })
                            .inner
                            .on_hover_text(hover.as_str())
                            .on_disabled_hover_text(hover.as_str())
                            .clicked();
                        if clicked {
                            acts.push(Act::Download(row.name.clone()));
                        }
                    }
                    RowAction::Cancel => {
                        if ui
                            .add_sized(button, egui::Button::new("Cancel"))
                            .on_hover_text("Stop the download; nothing of this atlas is kept")
                            .clicked()
                        {
                            acts.push(Act::Cancel);
                        }
                    }
                    RowAction::Remove { enabled } => {
                        let hover = self.remove_hover();
                        let clicked = ui
                            .add_enabled_ui(enabled, |ui| {
                                ui.add_sized(button, egui::Button::new("Remove"))
                            })
                            .inner
                            .on_hover_text(hover.as_str())
                            .on_disabled_hover_text(hover.as_str())
                            .clicked();
                        if clicked {
                            acts.push(Act::Remove(row.name.clone()));
                        }
                    }
                    RowAction::Nothing | RowAction::NotRemovable => {
                        ui.add_space(button[0] + ui.spacing().item_spacing.x);
                    }
                }
                let status = if action == RowAction::Cancel {
                    "downloading"
                } else if self.removing() == Some(row.name.as_str()) {
                    "removing"
                } else {
                    row.state.label()
                };
                let status_hover = match (&row.state, action) {
                    (AtlasState::Bundled(_), _) => {
                        "Comes with Wyss DBS Localizer; nothing to download".to_owned()
                    }
                    (_, RowAction::NotRemovable) => row.state.path().map_or_else(
                        String::new,
                        |p| {
                            format!(
                                "Already in {}; it was not downloaded here, so it is not removed here",
                                humanize_message(&p.display().to_string())
                            )
                        },
                    ),
                    (state, _) => state
                        .path()
                        .map(|p| humanize_message(&p.display().to_string()))
                        .unwrap_or_else(|| "Not on this computer yet".to_owned()),
                };
                ui.add_sized(
                    [unit * 5.5, h],
                    egui::Label::new(egui::RichText::new(status).color(pal.text_muted)),
                )
                .on_hover_text(status_hover);
                ui.add_sized([unit * 4.5, h], egui::Label::new(atlas_size(row.bytes)))
                    .on_hover_text("Size on disk once installed");
                ui.add(egui::Label::new(row.name.as_str()).truncate())
                    .on_hover_text(row.name.as_str());
            },
        );
        if action == RowAction::Cancel {
            if let Some(p) = self.progress() {
                let (frac, text) = progress_text(&row.name, &p);
                ui.add(egui::ProgressBar::new(frac).text(text));
            }
            if let Some(note) = self.note() {
                ui.add(
                    egui::Label::new(egui::RichText::new(note).small().color(pal.text_muted))
                        .wrap(),
                );
            }
        }
        if let Some(e) = self.error_for(&row.name) {
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(e).color(pal.err)).wrap());
                if ui
                    .button("Copy")
                    .on_hover_text("Copy the full message to the clipboard")
                    .clicked()
                {
                    ui.ctx().copy_text(format!("{}: {e}", row.name));
                }
            });
        }
        if self.is_freshly_installed(&row.name) && matches!(row.state, AtlasState::Installed { .. })
        {
            ui.add(egui::Label::new(egui::RichText::new(INSTALLED_NOTE).color(pal.ok)).wrap());
        }
    }
}

type Shared = Arc<Mutex<AtlasDownloadWindow>>;

fn store_id() -> egui::Id {
    egui::Id::new("atlas_download_window_state")
}

fn shared(ctx: &egui::Context) -> Shared {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(store_id()).clone())
}

fn lock(s: &Shared) -> MutexGuard<'_, AtlasDownloadWindow> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
pub fn install(ctx: &egui::Context, window: AtlasDownloadWindow) {
    ctx.data_mut(|d| d.insert_temp::<Shared>(store_id(), Arc::new(Mutex::new(window))));
}

pub fn open_window(ctx: &egui::Context) {
    let s = shared(ctx);
    lock(&s).open();
    ctx.request_repaint();
}

pub fn show_window(ctx: &egui::Context) {
    let s = shared(ctx);
    lock(&s).show(ctx);
}

pub fn running_download(ctx: &egui::Context) -> Option<String> {
    let s = shared(ctx);
    let w = lock(&s);
    w.downloading().map(str::to_owned)
}

#[cfg(test)]
#[path = "atlas_download_tests.rs"]
mod tests;
