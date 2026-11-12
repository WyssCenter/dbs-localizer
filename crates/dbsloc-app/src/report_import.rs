// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use dbsloc_pipeline::bids::Subject;
use dbsloc_pipeline::reports::{
    self, ImportOptions, ImportReport, IndexEntry, SessionReport, SnapshotChoice,
};
use dbsloc_pipeline::vat::settings::label_is_valid;
use eframe::egui;
use serde::Deserialize;

use crate::app::DbslocAppState;
use crate::panels::{em, info_icon, palette};

#[cfg(test)]
#[path = "report_import_tests.rs"]
mod tests;

/// Explanation under the *Anonymise* tick.
pub const ANONYMISE_INFO: &str = "Removes name, patient id, notes and serial number; shifts every date by a fixed random offset of at most 30 days.";

const MAX_CARD_ROWS: usize = 5;
const CACHE_RECHECK_SECONDS: f64 = 1.0;
const PREFIX_ERROR: &str = "The label prefix may only contain letters, digits, dots, hyphens and underscores, and at most 60 characters.";

/// What the dialog asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportImportAction {
    /// Lines for the Output panel.
    Log(Vec<String>),
    /// The import finished; `labels` are the stimulation labels that were written.
    Imported {
        /// Stimulation labels written by the import, in order.
        labels: Vec<String>,
        /// Subject the reports were imported into.
        subject: String,
    },
    /// The dialog was closed.
    Closed,
}

/// Which part of the dialog is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Review,
    Importing,
    Done,
}

/// What a key press means in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {
    None,
    Import,
    Cancel,
    StopImport,
    Close,
}

/// Maps Esc and Enter to the dialog action for the current stage.
pub(crate) fn key_action(stage: Stage, can_import: bool, escape: bool, enter: bool) -> KeyAction {
    if escape {
        return match stage {
            Stage::Review => KeyAction::Cancel,
            Stage::Importing => KeyAction::StopImport,
            Stage::Done => KeyAction::Close,
        };
    }
    if enter && stage == Stage::Review && can_import {
        return KeyAction::Import;
    }
    KeyAction::None
}

enum Parsed {
    Waiting,
    Ok(Box<SessionReport>),
    Failed(ParseFailure),
}

struct ParseFailure {
    message: String,
    first_lines: Option<Vec<String>>,
}

impl ParseFailure {
    fn plain(message: impl Into<String>) -> Self {
        ParseFailure {
            message: message.into(),
            first_lines: None,
        }
    }

    fn from_error(e: &reports::ReportImportError) -> Self {
        let first_lines = match e {
            reports::ReportImportError::Report(inner) => {
                inner.first_lines().map(<[String]>::to_vec)
            }
            _ => None,
        };
        ParseFailure {
            message: e.to_string(),
            first_lines,
        }
    }
}

struct FileEntry {
    path: PathBuf,
    name: String,
    parsed: Parsed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupTick {
    pub id: String,
    pub name: Option<String>,
    pub ticked: bool,
}

type ParseMsg = (usize, Result<SessionReport, ParseFailure>);

enum ImportMsg {
    Started,
    Done(usize, Box<Result<ImportReport, String>>),
    Finished,
}

/// The *Import session report* window.
pub struct ReportImportDialog {
    subject: Subject,
    files: Vec<FileEntry>,
    /// Remove identity and shift dates.
    pub anonymize: bool,
    /// Snapshot that yields stimulation labels.
    pub snapshot: SnapshotChoice,
    /// Optional prefix of every label.
    pub label_prefix: String,
    /// Keep a copy of the PDF in `sourcedata`.
    pub source_copy: bool,
    copy_saved: bool,
    groups: Vec<GroupTick>,
    parse_rx: Option<mpsc::Receiver<ParseMsg>>,
    import_rx: Option<mpsc::Receiver<ImportMsg>>,
    cancel: Arc<AtomicBool>,
    stage: Stage,
    progress: (usize, usize),
    outcomes: Vec<(bool, String)>,
    labels: Vec<String>,
    any_ok: bool,
    stopped: bool,
    key_file: Option<PathBuf>,
}

fn group_has_amplitude(group: &reports::model::Group) -> bool {
    [&group.hemispheres.left, &group.hemispheres.right]
        .into_iter()
        .flatten()
        .flat_map(|h| h.programs.iter())
        .any(|p| p.amplitude.is_some_and(|a| a > 0.0))
}

fn snapshot_groups(record: &SessionReport, choice: SnapshotChoice) -> Vec<&reports::model::Group> {
    match choice {
        SnapshotChoice::Final => record.snapshots.final_.groups.iter().collect(),
        SnapshotChoice::Initial => record.snapshots.initial.groups.iter().collect(),
        SnapshotChoice::Both => record
            .snapshots
            .final_
            .groups
            .iter()
            .chain(record.snapshots.initial.groups.iter())
            .collect(),
        SnapshotChoice::None => Vec::new(),
    }
}

/// Why `prefix` cannot start a label, or `None` when it is empty or valid.
pub fn prefix_error(prefix: &str) -> Option<String> {
    let p = prefix.trim();
    if p.is_empty() || label_is_valid(&format!("{p}_x")) && p.chars().count() <= 60 {
        None
    } else {
        Some(PREFIX_ERROR.to_owned())
    }
}

fn vendor_label(v: reports::model::Vendor) -> &'static str {
    match v {
        reports::model::Vendor::Medtronic => "Medtronic",
        reports::model::Vendor::BostonScientific => "Boston Scientific",
        reports::model::Vendor::Abbott => "Abbott",
    }
}

fn vendor_text(id: &str) -> String {
    match id {
        "medtronic" => "Medtronic".to_owned(),
        "boston_scientific" => "Boston Scientific".to_owned(),
        "abbott" => "Abbott".to_owned(),
        other => other.to_owned(),
    }
}

fn session_text(date: Option<&str>) -> String {
    date.map_or_else(|| "unknown".to_owned(), |d| d.replace('T', " "))
}

fn snapshot_name(choice: SnapshotChoice) -> &'static str {
    match choice {
        SnapshotChoice::Final => "final",
        SnapshotChoice::Initial => "initial",
        SnapshotChoice::Both => "final and initial",
        SnapshotChoice::None => "none",
    }
}

fn record_folder_text(record_path: &Path) -> String {
    let folder = record_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let vendor = record_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    match (vendor, folder) {
        (Some(v), Some(f)) => format!("{v}/{f}"),
        (None, Some(f)) => f,
        _ => record_path.display().to_string(),
    }
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// The Output-panel lines of one finished import.
pub(crate) fn import_lines(file: &str, result: &Result<ImportReport, String>) -> Vec<String> {
    match result {
        Ok(r) => {
            let mut lines = vec![format!(
                "dbsloc: note: report: {file}: record {}; labels {}; unchanged {}",
                record_folder_text(&r.record_path),
                join_or_none(&r.labels_written),
                join_or_none(&r.labels_unchanged),
            )];
            lines.extend(r.warnings.iter().cloned());
            for (label, reason) in &r.labels_failed {
                lines.push(format!(
                    "dbsloc: warning: report: {file}: label {label} not written: {reason}"
                ));
            }
            lines
        }
        Err(e) => vec![format!("dbsloc: error: report: {file}: {e}")],
    }
}

fn outcome_text(file: &str, result: &Result<ImportReport, String>) -> (bool, String) {
    match result {
        Ok(r) => {
            let mut text = format!(
                "{file}: {} written, {} unchanged",
                r.labels_written.len(),
                r.labels_unchanged.len()
            );
            if !r.labels_failed.is_empty() {
                text.push_str(&format!(", {} not written", r.labels_failed.len()));
            }
            (true, text)
        }
        Err(e) => (false, format!("{file}: {e}")),
    }
}

fn card_date(session: Option<&str>) -> String {
    session_text(session)
}

fn report_line(entry: &IndexEntry) -> String {
    let labels = if entry.labels.is_empty() {
        "no labels".to_owned()
    } else {
        entry.labels.join(", ")
    };
    format!(
        "{} · {} · {} · {}",
        card_date(entry.session.as_deref()),
        vendor_text(&entry.vendor),
        if entry.anonymized {
            "anonymised"
        } else {
            "not anonymised"
        },
        labels
    )
}

/// One line per report for the stimulation card (at most five, then a count).
pub fn reports_summary_lines(entries: &[IndexEntry]) -> Vec<String> {
    let mut lines: Vec<String> = entries
        .iter()
        .take(MAX_CARD_ROWS)
        .map(report_line)
        .collect();
    if entries.len() > MAX_CARD_ROWS {
        lines.push(format!("… and {} more", entries.len() - MAX_CARD_ROWS));
    }
    lines
}

impl ReportImportDialog {
    /// Opens the dialog on `paths` and starts parsing them on a worker thread.
    pub fn new(paths: Vec<PathBuf>, subject: Subject, ctx: &egui::Context) -> Self {
        let mut dialog = Self::without_worker(paths, subject);
        dialog.start_parsing(ctx);
        dialog
    }

    pub(crate) fn without_worker(paths: Vec<PathBuf>, subject: Subject) -> Self {
        let files = paths
            .into_iter()
            .map(|path| FileEntry {
                name: path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                path,
                parsed: Parsed::Waiting,
            })
            .collect();
        ReportImportDialog {
            subject,
            files,
            anonymize: false,
            snapshot: SnapshotChoice::Final,
            label_prefix: String::new(),
            source_copy: true,
            copy_saved: true,
            groups: Vec::new(),
            parse_rx: None,
            import_rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            stage: Stage::Review,
            progress: (0, 0),
            outcomes: Vec::new(),
            labels: Vec::new(),
            any_ok: false,
            stopped: false,
            key_file: None,
        }
    }

    fn start_parsing(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        let (tx, rx) = mpsc::channel();
        self.parse_rx = Some(rx);
        let repaint = ctx.clone();
        thread::spawn(move || {
            for (i, path) in paths.iter().enumerate() {
                let result = std::panic::catch_unwind(|| reports::inspect(path))
                    .unwrap_or_else(|_| {
                        Err(reports::ReportImportError::Io(
                            "the parser stopped unexpectedly".to_owned(),
                        ))
                    })
                    .map_err(|e| ParseFailure::from_error(&e));
                if tx.send((i, result)).is_err() {
                    return;
                }
                repaint.request_repaint();
            }
        });
    }

    /// Turns anonymising on or off; the copy tick follows (off while anonymising).
    pub fn set_anonymize(&mut self, on: bool) {
        if on == self.anonymize {
            return;
        }
        self.anonymize = on;
        if on {
            self.copy_saved = self.source_copy;
            self.source_copy = false;
        } else {
            self.source_copy = self.copy_saved;
        }
    }

    /// Chooses the snapshot and rebuilds the group ticks.
    pub fn set_snapshot(&mut self, choice: SnapshotChoice) {
        if choice == self.snapshot {
            return;
        }
        self.snapshot = choice;
        self.groups.clear();
        self.refresh_groups();
    }

    #[cfg(test)]
    pub(crate) fn groups(&self) -> &[GroupTick] {
        &self.groups
    }

    #[cfg(test)]
    pub(crate) fn set_group_ticked(&mut self, id: &str, ticked: bool) {
        for g in self.groups.iter_mut().filter(|g| g.id == id) {
            g.ticked = ticked;
        }
    }

    #[cfg(test)]
    pub(crate) fn stage(&self) -> Stage {
        self.stage
    }

    #[cfg(test)]
    pub(crate) fn set_report(&mut self, index: usize, record: SessionReport) {
        if let Some(f) = self.files.get_mut(index) {
            f.parsed = Parsed::Ok(Box::new(record));
        }
        self.refresh_groups();
    }

    fn refresh_groups(&mut self) {
        let mut fresh: Vec<GroupTick> = Vec::new();
        for file in &self.files {
            let Parsed::Ok(record) = &file.parsed else {
                continue;
            };
            for group in snapshot_groups(record, self.snapshot) {
                let has = group_has_amplitude(group);
                if let Some(existing) = fresh.iter_mut().find(|g| g.id == group.id) {
                    existing.ticked |= has;
                    if existing.name.is_none() {
                        existing.name.clone_from(&group.name);
                    }
                } else {
                    fresh.push(GroupTick {
                        id: group.id.clone(),
                        name: group.name.clone(),
                        ticked: has,
                    });
                }
            }
        }
        for g in &mut fresh {
            if let Some(old) = self.groups.iter().find(|o| o.id == g.id) {
                g.ticked = old.ticked;
            }
        }
        self.groups = fresh;
    }

    fn parsing(&self) -> bool {
        self.parse_rx.is_some()
    }

    fn parsed_count(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.parsed, Parsed::Ok(_)))
            .count()
    }

    /// Whether *Import* is enabled.
    pub fn can_import(&self) -> bool {
        self.stage == Stage::Review
            && !self.parsing()
            && self.parsed_count() > 0
            && prefix_error(&self.label_prefix).is_none()
    }

    /// The options the import runs with.
    pub fn options(&self) -> ImportOptions {
        let prefix = self.label_prefix.trim();
        let groups =
            (self.snapshot != SnapshotChoice::None && !self.groups.is_empty()).then(|| {
                self.groups
                    .iter()
                    .filter(|g| g.ticked)
                    .map(|g| g.id.clone())
                    .collect()
            });
        ImportOptions {
            anonymize: self.anonymize,
            snapshot: self.snapshot,
            groups,
            label_prefix: (!prefix.is_empty()).then(|| prefix.to_owned()),
            source_copy: self.source_copy && !self.anonymize,
            key_file: self.key_file.clone(),
        }
    }

    /// Starts the import of every parsed file on one worker thread.
    pub fn start_import(&mut self, ctx: &egui::Context) {
        if !self.can_import() {
            return;
        }
        let jobs: Vec<(usize, PathBuf)> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(f.parsed, Parsed::Ok(_)))
            .map(|(i, f)| (i, f.path.clone()))
            .collect();
        self.progress = (0, jobs.len());
        self.stage = Stage::Importing;
        self.cancel.store(false, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.import_rx = Some(rx);
        let subject = self.subject.clone();
        let opts = self.options();
        let cancel = Arc::clone(&self.cancel);
        let repaint = ctx.clone();
        thread::spawn(move || {
            for (i, path) in jobs {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let _ = tx.send(ImportMsg::Started);
                repaint.request_repaint();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    reports::import_report(&subject, &path, &opts)
                }))
                .unwrap_or_else(|_| {
                    Err(reports::ReportImportError::Io(
                        "the import stopped unexpectedly".to_owned(),
                    ))
                })
                .map_err(|e| e.to_string());
                if tx.send(ImportMsg::Done(i, Box::new(result))).is_err() {
                    return;
                }
                repaint.request_repaint();
            }
            let _ = tx.send(ImportMsg::Finished);
            repaint.request_repaint();
        });
    }

    /// Asks the import to stop after the file in progress.
    pub fn stop_import(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.stopped = true;
    }

    /// Collects the worker results; returns what the application has to do.
    pub fn poll(&mut self) -> Vec<ReportImportAction> {
        let mut actions = Vec::new();
        let mut parse_done = false;
        if let Some(rx) = &self.parse_rx {
            loop {
                match rx.try_recv() {
                    Ok((i, result)) => {
                        if let Some(f) = self.files.get_mut(i) {
                            f.parsed = match result {
                                Ok(r) => Parsed::Ok(Box::new(r)),
                                Err(e) => Parsed::Failed(e),
                            };
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        parse_done = true;
                        break;
                    }
                }
            }
        }
        if parse_done {
            self.parse_rx = None;
            for f in &mut self.files {
                if matches!(f.parsed, Parsed::Waiting) {
                    f.parsed = Parsed::Failed(ParseFailure::plain(
                        "E_REPORT_io: the file could not be read",
                    ));
                }
            }
        }
        self.refresh_groups();
        let mut finished = false;
        if let Some(rx) = &self.import_rx {
            loop {
                match rx.try_recv() {
                    Ok(ImportMsg::Started) => {
                        self.progress.0 = (self.progress.0 + 1).min(self.progress.1);
                    }
                    Ok(ImportMsg::Done(i, result)) => {
                        let result = *result;
                        let name = self
                            .files
                            .get(i)
                            .map_or_else(String::new, |f| f.name.clone());
                        actions.push(ReportImportAction::Log(import_lines(&name, &result)));
                        if let Ok(r) = &result {
                            self.any_ok = true;
                            self.labels.extend(r.labels_written.iter().cloned());
                        }
                        self.outcomes.push(outcome_text(&name, &result));
                    }
                    Ok(ImportMsg::Finished) | Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
        if finished {
            self.import_rx = None;
            self.stage = Stage::Done;
            if self.any_ok {
                actions.push(ReportImportAction::Imported {
                    labels: self.labels.clone(),
                    subject: self.subject.subj_id.clone(),
                });
            }
        }
        actions
    }

    fn file_ui(&self, ui: &mut egui::Ui, file: &FileEntry) {
        let pal = palette(ui);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(&file.name).strong());
            match &file.parsed {
                Parsed::Waiting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the report…");
                    });
                }
                Parsed::Failed(e) => {
                    ui.add(egui::Label::new(egui::RichText::new(&e.message).color(pal.err)).wrap());
                    if let Some(lines) = &e.first_lines {
                        let text = if lines.is_empty() {
                            "First lines of page 1: none".to_owned()
                        } else {
                            format!("First lines of page 1: {}", lines.join(" | "))
                        };
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                                .wrap(),
                        );
                    }
                }
                Parsed::Ok(r) => self.record_ui(ui, r),
            }
        });
    }

    fn record_ui(&self, ui: &mut egui::Ui, r: &SessionReport) {
        let pal = palette(ui);
        let muted = |t: String| egui::RichText::new(t).color(pal.text_muted);
        ui.label(format!(
            "{} · {}",
            vendor_label(r.vendor),
            r.report_kind.replace('_', " ")
        ));
        ui.label(muted(format!(
            "Session: {}",
            session_text(r.session.date.as_deref())
        )));
        let device = match (&r.device.model, &r.device.model_number) {
            (Some(m), Some(n)) => format!("{m} ({n})"),
            (Some(m), None) => m.clone(),
            (None, Some(n)) => n.clone(),
            (None, None) => "unknown".to_owned(),
        };
        ui.label(muted(format!("Device: {device}")));
        let leads: Vec<String> = r
            .leads
            .iter()
            .map(|l| {
                let side = match l.hemisphere {
                    reports::model::Hemisphere::Left => "left",
                    reports::model::Hemisphere::Right => "right",
                };
                let model = l
                    .catalogue_model
                    .as_deref()
                    .or(l.vendor_model.as_deref())
                    .unwrap_or("unknown model");
                format!("{side}: {model}")
            })
            .collect();
        ui.label(muted(format!(
            "Leads: {}",
            if leads.is_empty() {
                "none found".to_owned()
            } else {
                leads.join("; ")
            }
        )));
        if self.snapshot != SnapshotChoice::None {
            let groups: Vec<String> = snapshot_groups(r, self.snapshot)
                .into_iter()
                .map(|g| {
                    let mut t = g.id.clone();
                    if let Some(n) = g.name.as_deref().filter(|n| !n.is_empty()) {
                        t.push(' ');
                        t.push_str(n);
                    }
                    if g.active {
                        t.push_str(" (active)");
                    }
                    t
                })
                .collect();
            ui.add(
                egui::Label::new(muted(format!(
                    "Groups ({}): {}",
                    snapshot_name(self.snapshot),
                    if groups.is_empty() {
                        "none".to_owned()
                    } else {
                        groups.join(", ")
                    }
                )))
                .wrap(),
            );
        }
    }

    fn review_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        actions: &mut Vec<ReportImportAction>,
    ) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.label(format!(
            "Importing into sub-{}. Reports are read on this computer; nothing is sent anywhere.",
            self.subject.subj_id.trim_start_matches("sub-")
        ));
        ui.add_space(unit * 0.3);
        egui::ScrollArea::vertical()
            .id_salt("report_import_files")
            .max_height(unit * 14.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for file in &self.files {
                    self.file_ui(ui, file);
                }
            });
        ui.add_space(unit * 0.4);
        let mut anonymize = self.anonymize;
        ui.horizontal(|ui| {
            ui.checkbox(&mut anonymize, "Anonymise");
            info_icon(ui, ANONYMISE_INFO);
        });
        ui.label(egui::RichText::new(ANONYMISE_INFO).color(pal.text_muted));
        self.set_anonymize(anonymize);
        ui.add_space(unit * 0.3);
        let mut choice = self.snapshot;
        egui::ComboBox::from_label("Create stimulation settings from")
            .selected_text(match choice {
                SnapshotChoice::Final => "Final",
                SnapshotChoice::Initial => "Initial",
                SnapshotChoice::Both => "Both",
                SnapshotChoice::None => "None",
            })
            .show_ui(ui, |ui| {
                for (value, text) in [
                    (SnapshotChoice::Final, "Final"),
                    (SnapshotChoice::Initial, "Initial"),
                    (SnapshotChoice::Both, "Both"),
                    (SnapshotChoice::None, "None"),
                ] {
                    ui.selectable_value(&mut choice, value, text);
                }
            })
            .response
            .on_hover_text(
                "Which settings printed in the report become stimulation settings of this subject. Final is the state at the end of the session, Initial the state when the session started.",
            );
        self.set_snapshot(choice);
        if self.snapshot != SnapshotChoice::None && !self.groups.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Groups:");
                for g in &mut self.groups {
                    let text = match g.name.as_deref().filter(|n| !n.is_empty()) {
                        Some(n) => format!("{} {n}", g.id),
                        None => g.id.clone(),
                    };
                    ui.checkbox(&mut g.ticked, text);
                }
                info_icon(
                    ui,
                    "Each ticked group becomes one stimulation label. Groups with a program above 0 amplitude are ticked at first. A tick applies to every file with a group of that letter.",
                );
            });
        }
        ui.horizontal(|ui| {
            let label = ui.label("Label prefix");
            let edit = egui::TextEdit::singleline(&mut self.label_prefix)
                .hint_text("optional")
                .desired_width(unit * 10.0);
            ui.add(edit).labelled_by(label.id).on_hover_text(
                "Written in front of every stimulation label, for example the name of the visit",
            );
        });
        if let Some(e) = prefix_error(&self.label_prefix) {
            ui.add(egui::Label::new(egui::RichText::new(e).color(pal.err)).wrap());
        }
        ui.add_enabled_ui(!self.anonymize, |ui| {
            ui.checkbox(&mut self.source_copy, "Keep a copy of the PDF in sourcedata")
                .on_hover_text("Stores the original PDF next to the subject's source data. Not available when anonymising, because the file may carry the patient's name.");
        });
        ui.add_space(unit * 0.5);
        ui.horizontal(|ui| {
            let import = ui
                .add_enabled(self.can_import(), egui::Button::new("Import"))
                .on_hover_text("Read the reports into this subject (Enter)")
                .on_disabled_hover_text(if self.parsing() {
                    "Wait until every report has been read"
                } else if prefix_error(&self.label_prefix).is_some() {
                    "Fix the label prefix first"
                } else {
                    "No report could be read"
                });
            if import.clicked() {
                self.start_import(ctx);
            }
            if ui
                .button("Cancel")
                .on_hover_text("Close without importing (Esc)")
                .clicked()
            {
                actions.push(ReportImportAction::Closed);
            }
            if self.parsing() {
                ui.spinner();
            }
        });
    }

    fn importing_ui(&mut self, ui: &mut egui::Ui) {
        let (n, total) = self.progress;
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Importing {n} of {total}…"));
        });
        ui.add(egui::ProgressBar::new(if total == 0 {
            0.0
        } else {
            (n.saturating_sub(1)) as f32 / total as f32
        }));
        if self.stopped {
            ui.label("Stopping after the current file…");
        }
        if ui
            .add_enabled(!self.stopped, egui::Button::new("Cancel"))
            .on_hover_text("Finish the file in progress, then stop (Esc)")
            .clicked()
        {
            self.stop_import();
        }
    }

    fn done_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ReportImportAction>) {
        let pal = palette(ui);
        ui.label(
            egui::RichText::new(if self.stopped {
                "Import stopped"
            } else {
                "Import finished"
            })
            .strong(),
        );
        egui::ScrollArea::vertical()
            .id_salt("report_import_summary")
            .max_height(em(ui) * 12.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (ok, text) in &self.outcomes {
                    let color = if *ok { pal.text } else { pal.err };
                    ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
                }
                if !self.labels.is_empty() {
                    ui.label(format!(
                        "New stimulation labels: {}",
                        self.labels.join(", ")
                    ));
                }
            });
        if ui
            .button("Close")
            .on_hover_text("Close this window (Esc)")
            .clicked()
        {
            actions.push(ReportImportAction::Closed);
        }
    }

    /// Draws the window and returns what the application has to do.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<ReportImportAction> {
        let mut actions = self.poll();
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let enter = !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        match key_action(self.stage, self.can_import(), escape, enter) {
            KeyAction::None => {}
            KeyAction::Import => self.start_import(ctx),
            KeyAction::Cancel | KeyAction::Close => actions.push(ReportImportAction::Closed),
            KeyAction::StopImport => self.stop_import(),
        }
        let unit = ctx.global_style().spacing.interact_size.y;
        egui::Window::new("Import session report")
            .id(egui::Id::new("report_import_window"))
            .collapsible(false)
            .resizable(true)
            .min_width(unit * 28.0)
            .default_width(unit * 30.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match self.stage {
                Stage::Review => self.review_ui(ui, ctx, &mut actions),
                Stage::Importing => self.importing_ui(ui),
                Stage::Done => self.done_ui(ui, &mut actions),
            });
        actions
    }
}

struct RecordView {
    title: String,
    text: String,
}

/// Owns the import dialog and the record window of the application.
#[derive(Default)]
pub struct ReportImportHost {
    dialog: Option<ReportImportDialog>,
    record: Option<RecordView>,
}

impl ReportImportHost {
    /// Opens the dialog on `paths` for `subject`.
    pub fn open(&mut self, paths: Vec<PathBuf>, subject: Subject, ctx: &egui::Context) {
        self.dialog = Some(ReportImportDialog::new(paths, subject, ctx));
    }

    /// Whether the dialog is open.
    pub fn is_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// Whether the dialog has read every file (for screenshots).
    pub fn is_settled(&self) -> bool {
        self.dialog.as_ref().is_some_and(|d| !d.parsing())
    }

    /// Draws the dialog and the record window; returns what the application has to do.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<ReportImportAction> {
        if let Some(request) = ctx.data_mut(|d| d.remove_temp::<ShowRecordRequest>(show_id())) {
            let text = std::fs::read_to_string(&request.path)
                .unwrap_or_else(|e| format!("The record could not be read: {e}"));
            self.record = Some(RecordView {
                title: request.title,
                text,
            });
        }
        let mut actions = Vec::new();
        if let Some(dialog) = &mut self.dialog {
            actions = dialog.show(ctx);
            if actions.contains(&ReportImportAction::Closed) {
                self.dialog = None;
            }
        }
        if let Some(view) = &self.record {
            let mut open = true;
            let screen = ctx.content_rect().size();
            egui::Window::new(view.title.clone())
                .id(egui::Id::new("report_record_window"))
                .open(&mut open)
                .resizable(true)
                .default_size(screen * 0.5)
                .show(ctx, |ui| {
                    egui::ScrollArea::both()
                        .id_salt("report_record_text")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut text: &str = &view.text;
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                });
            if !open {
                self.record = None;
            }
        }
        actions
    }
}

#[derive(Clone, Default)]
struct ShowRecordRequest {
    path: PathBuf,
    title: String,
}

fn import_id() -> egui::Id {
    egui::Id::new("report_import_request")
}

fn show_id() -> egui::Id {
    egui::Id::new("report_show_record_request")
}

fn cache_id() -> egui::Id {
    egui::Id::new("report_index_cache")
}

/// Asks the application to start an import (file dialog, then the window).
pub(crate) fn request_import(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(import_id(), true));
    ctx.request_repaint();
}

/// Takes a pending request from [`request_import`].
pub(crate) fn take_import_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(import_id()))
        .is_some()
}

fn request_show_record(ctx: &egui::Context, path: PathBuf, title: String) {
    ctx.data_mut(|d| d.insert_temp(show_id(), ShowRecordRequest { path, title }));
    ctx.request_repaint();
}

/// Forgets the cached report list, so the next frame reads the index again.
pub(crate) fn invalidate_reports_cache(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove_temp::<ReportsCache>(cache_id()));
}

#[derive(Clone, Default)]
struct ReportsCache {
    dir: PathBuf,
    checked_at: f64,
    mtime: Option<std::time::SystemTime>,
    entries: Arc<Vec<IndexEntry>>,
}

#[derive(Deserialize, Default)]
struct IndexFile {
    #[serde(default)]
    reports: Vec<IndexEntry>,
}

fn index_file(subj_dir: &Path) -> PathBuf {
    subj_dir.join("clinical").join("reports").join("index.json")
}

fn read_entries(subj_dir: &Path) -> Vec<IndexEntry> {
    std::fs::read_to_string(index_file(subj_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<IndexFile>(&t).ok())
        .unwrap_or_default()
        .reports
}

fn cached_entries(ctx: &egui::Context, subj_dir: &Path) -> Arc<Vec<IndexEntry>> {
    let now = ctx.input(|i| i.time);
    let cache = ctx.data(|d| d.get_temp::<ReportsCache>(cache_id()));
    if let Some(c) = &cache {
        if c.dir == subj_dir && now - c.checked_at < CACHE_RECHECK_SECONDS {
            return Arc::clone(&c.entries);
        }
    }
    let mtime = std::fs::metadata(index_file(subj_dir))
        .and_then(|m| m.modified())
        .ok();
    let entries = match cache {
        Some(c) if c.dir == subj_dir && c.mtime == mtime => c.entries,
        _ => Arc::new(read_entries(subj_dir)),
    };
    ctx.data_mut(|d| {
        d.insert_temp(
            cache_id(),
            ReportsCache {
                dir: subj_dir.to_path_buf(),
                checked_at: now,
                mtime,
                entries: Arc::clone(&entries),
            },
        );
    });
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(CACHE_RECHECK_SECONDS));
    entries
}

/// The *Reports* line of the *Volume of activated tissue* card.
pub(crate) fn reports_card_section(ui: &mut egui::Ui, state: &DbslocAppState) {
    let Some(folder) = state
        .selected_subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with("sub-") {
                s.to_owned()
            } else {
                format!("sub-{s}")
            }
        })
    else {
        return;
    };
    let Some(subj_dir) = state.subject_derivatives_dir(&folder) else {
        return;
    };
    let entries = cached_entries(ui.ctx(), &subj_dir);
    let pal = palette(ui);
    let reports_dir = subj_dir.join("clinical").join("reports");
    let error_id = egui::Id::new(("reports_open_folder_error", &subj_dir));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Reports").color(pal.text_muted));
        if !entries.is_empty()
            && crate::panels::open_folder_button(
                ui,
                "Open the folder with the imported session reports (the records and their text copies) in the file manager",
            )
        {
            let result = crate::panels::reveal_in_file_manager(&reports_dir);
            ui.ctx().data_mut(|d| match result {
                Ok(()) => {
                    d.remove_temp::<String>(error_id);
                }
                Err(e) => {
                    d.insert_temp(error_id, e);
                }
            });
        }
    });
    if let Some(e) = ui.ctx().data(|d| d.get_temp::<String>(error_id)) {
        ui.label(egui::RichText::new(e).small().color(pal.err));
    }
    if entries.is_empty() {
        ui.label(egui::RichText::new("None imported yet").color(pal.text_muted));
        return;
    }
    let lines = reports_summary_lines(&entries);
    for (i, line) in lines.iter().enumerate() {
        match entries.get(i).filter(|_| i < MAX_CARD_ROWS) {
            Some(entry) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(line);
                    if ui
                        .small_button("Show…")
                        .on_hover_text("Show the stored record of this report")
                        .clicked()
                    {
                        let record = subj_dir
                            .join("clinical")
                            .join("reports")
                            .join(&entry.folder)
                            .join(format!("{folder}_desc-sessionreport.json"));
                        let date = entry
                            .session
                            .as_deref()
                            .map_or("unknown date", |s| s.get(..10).unwrap_or(s));
                        request_show_record(
                            ui.ctx(),
                            record,
                            format!("{folder} session report {date}"),
                        );
                    }
                });
            }
            None => {
                ui.label(egui::RichText::new(line).color(pal.text_muted));
            }
        }
    }
}
