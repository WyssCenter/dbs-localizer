// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbsloc_pipeline::methods_text;
use eframe::egui;

use crate::panels::{em, palette, reveal_in_file_manager};

pub(crate) const INTRO: &str =
    "One paragraph per step that ran, with references. Copy it into the methods section of a paper.";
pub(crate) const LATEST_ONLY_LABEL: &str = "Latest run of each step only";
pub(crate) const LATEST_ONLY_HOVER: &str =
    "Show only the most recent entry of each step. Untick to see every run, oldest first.";
pub(crate) const MISSING_MESSAGE: &str =
    "There is no methods text for this subject yet because no step has run.";
pub(crate) const NO_SUBJECT_MESSAGE: &str = "No subject is selected.";
pub(crate) const NO_ENTRY_MESSAGE: &str =
    "The file holds no entry in the standard layout. Untick the box above to see all of it.";
const COPIED_FOR: Duration = Duration::from_secs(2);

pub(crate) struct MethodsWindow {
    pub open: bool,
    pub latest_only: bool,
    loaded_for: Option<PathBuf>,
    raw: Vec<u8>,
    error: Option<String>,
    shown: String,
    notice: Option<String>,
    copied_at: Option<Instant>,
}

impl Default for MethodsWindow {
    fn default() -> Self {
        Self {
            open: false,
            latest_only: true,
            loaded_for: None,
            raw: Vec::new(),
            error: None,
            shown: String::new(),
            notice: None,
            copied_at: None,
        }
    }
}

pub(crate) fn window_title(subject: &str) -> String {
    let id = subject.strip_prefix("sub-").unwrap_or(subject);
    format!("Methods text — sub-{id}")
}

impl MethodsWindow {
    pub(crate) fn open_for(&mut self, path: Option<&Path>) {
        self.open = true;
        self.reload_from(path);
    }

    pub(crate) fn sync_subject(&mut self, path: Option<&Path>) {
        if self.open && self.loaded_for.as_deref() != path {
            self.reload_from(path);
        }
    }

    pub(crate) fn reload_if_open(&mut self) {
        if self.open {
            let path = self.loaded_for.clone();
            self.reload_from(path.as_deref());
        }
    }

    pub(crate) fn refresh(&mut self) {
        let path = self.loaded_for.clone();
        self.reload_from(path.as_deref());
    }

    pub(crate) fn set_latest_only(&mut self, latest_only: bool) {
        if self.latest_only != latest_only {
            self.latest_only = latest_only;
            self.rebuild();
        }
    }

    #[cfg(test)]
    pub(crate) fn shown(&self) -> &str {
        &self.shown
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn loaded_for(&self) -> Option<&Path> {
        self.loaded_for.as_deref()
    }

    fn reload_from(&mut self, path: Option<&Path>) {
        self.loaded_for = path.map(Path::to_path_buf);
        self.notice = None;
        self.raw.clear();
        self.error = None;
        match path {
            None => self.error = Some(NO_SUBJECT_MESSAGE.to_owned()),
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => self.raw = bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.error = Some(MISSING_MESSAGE.to_owned());
                }
                Err(e) => {
                    self.error = Some(format!("The methods text could not be read: {e}."));
                }
            },
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        if self.error.is_some() {
            self.shown.clear();
            return;
        }
        let rendered = methods_text::render_bytes(&self.raw, self.latest_only);
        let text = String::from_utf8_lossy(&rendered);
        self.shown = text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned();
    }

    fn empty_note(&self) -> Option<&'static str> {
        (self.error.is_none() && self.shown.is_empty() && !self.raw.is_empty())
            .then_some(NO_ENTRY_MESSAGE)
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, subject: &str, path: Option<&Path>) {
        if !self.open {
            return;
        }
        self.sync_subject(path);
        let mut open = true;
        let width = egui::TextStyle::Body.resolve(&ctx.global_style()).size;
        let mut copy = false;
        let mut reveal = false;
        let mut refresh = false;
        let mut latest = self.latest_only;
        egui::Window::new(window_title(subject))
            .id(egui::Id::new("methods_text_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(width * 44.0)
            .default_height(width * 30.0)
            .show(ctx, |ui| {
                let unit = em(ui);
                let pal = palette(ui);
                ui.label(egui::RichText::new(INTRO).small().color(pal.text_muted));
                ui.add_space(unit * 0.3);
                ui.checkbox(&mut latest, LATEST_ONLY_LABEL)
                    .on_hover_text(LATEST_ONLY_HOVER);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.error.is_none(), egui::Button::new("Copy"))
                        .on_hover_text("Copy the text shown below to the clipboard")
                        .clicked()
                    {
                        copy = true;
                    }
                    if ui
                        .button(crate::panels::OPEN_FOLDER_LABEL)
                        .on_hover_text(
                            "Open the folder with the methods text file in the file manager",
                        )
                        .clicked()
                    {
                        reveal = true;
                    }
                    if ui
                        .button("Refresh")
                        .on_hover_text("Read the methods text file again")
                        .clicked()
                    {
                        refresh = true;
                    }
                    if self.copied_at.is_some_and(|t| t.elapsed() < COPIED_FOR) {
                        ui.label(egui::RichText::new("Copied").color(pal.text_muted));
                        ui.ctx().request_repaint_after(COPIED_FOR);
                    }
                });
                if let Some(notice) = &self.notice {
                    ui.label(egui::RichText::new(notice).color(pal.text_muted));
                }
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(error) = &self.error {
                            ui.label(error);
                        } else if let Some(note) = self.empty_note() {
                            ui.label(note);
                        } else {
                            let mut text: &str = &self.shown;
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(8)
                                    .id_salt("methods_text_view"),
                            );
                        }
                    });
            });
        self.set_latest_only(latest);
        if copy {
            ctx.copy_text(self.shown.clone());
            self.copied_at = Some(Instant::now());
        }
        if reveal {
            match self.loaded_for.as_deref().and_then(Path::parent) {
                Some(dir) if dir.is_dir() => {
                    if let Err(e) = reveal_in_file_manager(dir) {
                        self.notice = Some(format!("The folder could not be opened: {e}"));
                    }
                }
                _ => {
                    self.notice = Some("The folder does not exist yet.".to_owned());
                }
            }
        }
        if refresh {
            self.refresh();
        }
        if !open {
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A1: &str = "[2026-01-01T00:00:00Z] preprocess | Wyss DBS Localizer 1\nFirst.\n\n";
    const B1: &str = "[2026-01-02T00:00:00Z] coreg | Wyss DBS Localizer 1\nCoreg.\n\n";
    const A2: &str = "[2026-01-03T00:00:00Z] preprocess | Wyss DBS Localizer 1\nSecond.\n\n";

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("dbsloc_methods_win_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_title_names_the_subject_with_one_prefix() {
        assert_eq!(window_title("sub-01"), "Methods text — sub-01");
        assert_eq!(window_title("01"), "Methods text — sub-01");
    }

    #[test]
    fn opening_loads_the_file_and_defaults_to_the_latest_run_of_each_step() {
        let dir = scratch("load");
        let file = dir.join("methods.txt");
        std::fs::write(&file, format!("{A1}{B1}{A2}")).unwrap();
        let mut w = MethodsWindow::default();
        assert!(w.latest_only && !w.open);
        w.open_for(Some(&file));
        assert!(w.open);
        assert_eq!(w.error(), None);
        assert_eq!(w.shown(), format!("{B1}{A2}"));
        assert_eq!(w.loaded_for(), Some(file.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggling_the_checkbox_changes_the_text_without_reading_the_file_again() {
        let dir = scratch("toggle");
        let file = dir.join("methods.txt");
        std::fs::write(&file, format!("{A1}{B1}{A2}")).unwrap();
        let mut w = MethodsWindow::default();
        w.open_for(Some(&file));
        std::fs::write(&file, "changed on disk").unwrap();
        w.set_latest_only(false);
        assert_eq!(w.shown(), format!("{A1}{B1}{A2}"));
        w.set_latest_only(true);
        assert_eq!(w.shown(), format!("{B1}{A2}"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_shows_one_plain_sentence_and_no_text() {
        let dir = scratch("missing");
        let mut w = MethodsWindow::default();
        w.open_for(Some(&dir.join("absent.txt")));
        assert_eq!(w.error(), Some(MISSING_MESSAGE));
        assert!(w.shown().is_empty());
        w.open_for(None);
        assert_eq!(w.error(), Some(NO_SUBJECT_MESSAGE));
        let unreadable = dir.join("a_folder");
        std::fs::create_dir_all(&unreadable).unwrap();
        w.open_for(Some(&unreadable));
        assert!(w
            .error()
            .is_some_and(|e| e.starts_with("The methods text could not be read: ")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_subject_change_reloads_only_while_the_window_is_open() {
        let dir = scratch("subject");
        let one = dir.join("one.txt");
        let two = dir.join("two.txt");
        std::fs::write(&one, A1).unwrap();
        std::fs::write(&two, B1).unwrap();
        let mut w = MethodsWindow::default();
        w.sync_subject(Some(&one));
        assert!(w.shown().is_empty() && w.loaded_for().is_none());
        w.open_for(Some(&one));
        assert_eq!(w.shown(), A1);
        w.sync_subject(Some(&one));
        std::fs::write(&one, "edited").unwrap();
        w.sync_subject(Some(&one));
        assert_eq!(w.shown(), A1);
        w.sync_subject(Some(&two));
        assert_eq!(w.shown(), B1);
        assert_eq!(w.loaded_for(), Some(two.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_run_and_the_refresh_button_read_the_file_again() {
        let dir = scratch("finished");
        let file = dir.join("methods.txt");
        let mut w = MethodsWindow::default();
        w.open_for(Some(&file));
        assert_eq!(w.error(), Some(MISSING_MESSAGE));
        std::fs::write(&file, A1).unwrap();
        w.reload_if_open();
        assert_eq!(w.error(), None);
        assert_eq!(w.shown(), A1);
        std::fs::write(&file, format!("{A1}{B1}")).unwrap();
        w.refresh();
        assert_eq!(w.shown(), format!("{A1}{B1}"));
        w.open = false;
        std::fs::write(&file, A2).unwrap();
        w.reload_if_open();
        assert_eq!(w.shown(), format!("{A1}{B1}"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_bytes_a_byte_order_mark_and_a_file_without_entries_are_handled() {
        let dir = scratch("odd");
        let file = dir.join("methods.txt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"note \xFF\n");
        bytes.extend_from_slice(A1.as_bytes());
        std::fs::write(&file, &bytes).unwrap();
        let mut w = MethodsWindow::default();
        w.open_for(Some(&file));
        assert_eq!(w.shown(), A1);
        w.set_latest_only(false);
        assert!(w.shown().starts_with("note \u{fffd}"), "{:?}", w.shown());
        std::fs::write(&file, "only a note\n").unwrap();
        w.refresh();
        assert_eq!(w.empty_note(), None);
        w.set_latest_only(true);
        assert!(w.shown().is_empty());
        assert_eq!(w.empty_note(), Some(NO_ENTRY_MESSAGE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
