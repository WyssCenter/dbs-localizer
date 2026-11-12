// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The start screen: recent subjects and folders, open a new one.
//!
//! Everything is a button or a list row with a tooltip; the only visible words
//! are the labels. The screen decides nothing itself — it returns a
//! [`LandingAction`] and the application changes the workspace.

use std::path::Path;

use eframe::egui;

use crate::app::{DbslocAppState, RecentSubject};
use crate::guided::{centered_column, primary_button_sized};
use crate::panels::{em, palette, subject_rows, SubjectPanel};

const CARD_EM: f32 = 44.0;
const LIST_ROWS: usize = 6;

/// What the landing screen remembers between frames, never persisted.
#[derive(Default)]
pub struct LandingView {
    /// The subject list of the current folder is expanded.
    pub subjects_open: bool,
    pub import_block: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandingAction {
    None,
    PickFolder,
    OpenFolder(String),
    OpenSubject { bids_dir: String, subject: String },
    ImportImages,
    Skip,
}

/// Drop one recent subject from the list.
pub fn forget_subject_in_state(state: &mut DbslocAppState, target: &RecentSubject) {
    state.recent_subjects.retain(|r| r != target);
}

/// Drop one recent folder, and the recent subjects that came from it.
pub fn forget_folder_in_state(state: &mut DbslocAppState, folder: &str) {
    state.recent_bids_dirs.retain(|d| d != folder);
    state.recent_subjects.retain(|r| r.bids_dir != folder);
}

fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

fn parent_path(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Default)]
struct RowOutcome {
    open: bool,
    forget: bool,
}

/// One list row: `primary` on the left, `secondary` muted on the right, a ✖ on
/// hover and a context menu to forget it.
fn list_row(
    ui: &mut egui::Ui,
    id_salt: &str,
    primary: &str,
    secondary: &str,
    emphasised: bool,
    hover: &str,
) -> RowOutcome {
    let pal = palette(ui);
    let height = em(ui) * 1.9;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let forget_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - height * 0.5, rect.center().y),
        egui::vec2(height * 0.8, height * 0.8),
    );
    let forget = ui.interact(forget_rect, response.id.with(id_salt), egui::Sense::click());
    let hovered = response.hovered() || forget.hovered() || response.has_focus();
    let painter = ui.painter().with_clip_rect(rect);
    if hovered {
        painter.rect_filled(rect, 4.0, pal.hairline);
    }
    let pad = height * 0.3;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let primary_galley = painter.layout_no_wrap(
        primary.to_owned(),
        font,
        if emphasised { pal.accent } else { pal.text },
    );
    let primary_w = primary_galley.size().x;
    painter.galley(
        egui::pos2(
            rect.left() + pad,
            rect.center().y - primary_galley.size().y / 2.0,
        ),
        primary_galley,
        pal.text,
    );
    let room = rect.width() - primary_w - pad * 3.0 - height;
    if !secondary.is_empty() && room > em(ui) * 3.0 {
        let mut job = egui::text::LayoutJob::single_section(
            secondary.to_owned(),
            egui::TextFormat::simple(small, pal.text_muted),
        );
        job.wrap.max_width = room;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let galley = painter.layout_job(job);
        let x = rect.right() - height - galley.size().x;
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text_muted,
        );
    }
    if hovered {
        painter.text(
            forget_rect.center(),
            egui::Align2::CENTER_CENTER,
            "✖",
            egui::TextStyle::Small.resolve(ui.style()),
            pal.text_muted,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, primary));
    let forget_clicked = forget.clicked();
    let mut menu_forget = false;
    forget.on_hover_text("Forget this entry");
    response.context_menu(|ui| {
        if ui.button("Forget").clicked() {
            menu_forget = true;
            ui.close();
        }
    });
    let open = response.on_hover_text(hover).clicked() && !forget_clicked;
    RowOutcome {
        open,
        forget: forget_clicked || menu_forget,
    }
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    let pal = palette(ui);
    ui.add(
        egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted)).selectable(false),
    );
}

fn wide_button(
    ui: &mut egui::Ui,
    label: &str,
    primary: bool,
    enabled: bool,
    hover: &str,
) -> egui::Response {
    let unit = em(ui);
    let size = egui::vec2(ui.available_width(), unit * 2.4);
    if primary {
        primary_button_sized(ui, label, enabled, hover, size)
    } else {
        ui.add_enabled(enabled, egui::Button::new(label).min_size(size))
            .on_hover_text(hover)
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    subject_panel: &SubjectPanel,
    view: &mut LandingView,
    logo: &egui::TextureHandle,
    gpu_help: &crate::gpu_status::GpuHelp,
) -> LandingAction {
    let unit = em(ui);
    let pal = palette(ui);
    let mut action = LandingAction::None;

    ui.add_space((ui.available_height() * 0.08).max(unit));
    centered_column(ui, CARD_EM, |ui| {
        crate::app_log::notice(ui);
        crate::gpu_status::notice(ui, gpu_help);
        egui::Frame::new()
            .fill(pal.surface_raised)
            .stroke(egui::Stroke::new(1.0_f32, pal.hairline))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same((unit * 1.4) as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    crate::app::show_logo_sized(ui, logo, unit * 2.6);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("DBS Localizer")
                                .heading()
                                .strong()
                                .size(26.0),
                        )
                        .selectable(false),
                    );
                });
                ui.add_space(unit * 0.8);
                ui.columns(2, |cols| {
                    recent_column(&mut cols[0], state, &mut action);
                    open_column(&mut cols[1], state, subject_panel, view, &mut action);
                });
                ui.add_space(unit * 0.8);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.guided_default, "Guide me through the steps")
                        .on_hover_text(
                            "After opening a subject, go through the steps one at a time",
                        );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .link("Skip")
                            .on_hover_text("Go straight to the full workspace")
                            .clicked()
                        {
                            action = LandingAction::Skip;
                        }
                    });
                });
            });
    });
    action
}

fn recent_column(ui: &mut egui::Ui, state: &mut DbslocAppState, action: &mut LandingAction) {
    let pal = palette(ui);
    if state.recent_subjects.is_empty() && state.recent_bids_dirs.is_empty() {
        ui.add(
            egui::Label::new(egui::RichText::new("Nothing opened yet").color(pal.text_muted))
                .selectable(false),
        );
        return;
    }

    let mut forget_subject: Option<RecentSubject> = None;
    let mut forget_folder: Option<String> = None;
    if !state.recent_subjects.is_empty() {
        section_label(ui, "Recent subjects");
        for recent in &state.recent_subjects {
            let outcome = list_row(
                ui,
                "subject",
                &recent.subject,
                &folder_name(&recent.bids_dir),
                false,
                &format!("{} — {}", recent.subject, recent.bids_dir),
            );
            if outcome.open {
                *action = LandingAction::OpenSubject {
                    bids_dir: recent.bids_dir.clone(),
                    subject: recent.subject.clone(),
                };
            }
            if outcome.forget {
                forget_subject = Some(recent.clone());
            }
        }
    }
    if !state.recent_bids_dirs.is_empty() {
        ui.add_space(em(ui) * 0.5);
        section_label(ui, "Recent folders");
        for dir in &state.recent_bids_dirs {
            let outcome = list_row(
                ui,
                "folder",
                &folder_name(dir),
                &parent_path(dir),
                *dir == state.bids_dir,
                dir,
            );
            if outcome.open {
                *action = LandingAction::OpenFolder(dir.clone());
            }
            if outcome.forget {
                forget_folder = Some(dir.clone());
            }
        }
    }
    if let Some(target) = forget_subject {
        forget_subject_in_state(state, &target);
    }
    if let Some(folder) = forget_folder {
        forget_folder_in_state(state, &folder);
    }
}

fn open_column(
    ui: &mut egui::Ui,
    state: &DbslocAppState,
    subject_panel: &SubjectPanel,
    view: &mut LandingView,
    action: &mut LandingAction,
) {
    let unit = em(ui);
    let has_folder = !state.bids_dir.trim().is_empty();
    if wide_button(
        ui,
        "Open input directory…",
        true,
        true,
        "Choose an input directory (Ctrl+O)",
    )
    .clicked()
    {
        *action = LandingAction::PickFolder;
    }
    ui.add_space(unit * 0.3);
    if wide_button(
        ui,
        "Open BIDS subject…",
        false,
        has_folder,
        if has_folder {
            "Pick a subject of the current folder"
        } else {
            "Choose an input directory first"
        },
    )
    .clicked()
    {
        view.subjects_open = !view.subjects_open;
    }
    ui.add_space(unit * 0.3);
    if wide_button(
        ui,
        "Import raw NIfTI / DICOM…",
        false,
        view.import_block.is_none(),
        "Create a subject from NIfTI files or DICOM folders (Ctrl+I)",
    )
    .on_disabled_hover_text(view.import_block.unwrap_or_default())
    .clicked()
    {
        *action = LandingAction::ImportImages;
    }
    if !(has_folder && view.subjects_open) {
        return;
    }
    ui.add_space(unit * 0.4);
    section_label(ui, &folder_name(&state.bids_dir));
    if subject_panel.is_scanning() {
        ui.add(egui::Spinner::new().size(unit));
        return;
    }
    let subjects = subject_panel.subjects();
    if subjects.is_empty() {
        section_label(ui, "No subjects");
        return;
    }
    let row_h = unit + 2.0 * ui.spacing().button_padding.y + ui.spacing().item_spacing.y;
    let height = row_h * subjects.len().min(LIST_ROWS) as f32;
    let picked = ui
        .allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            subject_rows(ui, "landing_subjects", subjects, None, "Open this subject")
        })
        .inner;
    if let Some(subject) = picked {
        *action = LandingAction::OpenSubject {
            bids_dir: state.bids_dir.trim().to_owned(),
            subject,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::remember_subject_in_state;

    #[test]
    fn forgetting_a_folder_drops_its_subjects_too() {
        let mut state = DbslocAppState {
            recent_bids_dirs: vec!["a".to_owned(), "b".to_owned()],
            ..DbslocAppState::default()
        };
        remember_subject_in_state(&mut state, "a", "sub-01");
        remember_subject_in_state(&mut state, "b", "sub-02");
        forget_folder_in_state(&mut state, "a");
        assert_eq!(state.recent_bids_dirs, vec!["b".to_owned()]);
        assert_eq!(state.recent_subjects.len(), 1);
        assert_eq!(state.recent_subjects[0].subject, "sub-02");
    }

    #[test]
    fn forgetting_one_subject_keeps_the_others() {
        let mut state = DbslocAppState::default();
        remember_subject_in_state(&mut state, "a", "sub-01");
        remember_subject_in_state(&mut state, "a", "sub-02");
        let target = state.recent_subjects[0].clone();
        forget_subject_in_state(&mut state, &target);
        assert_eq!(state.recent_subjects.len(), 1);
        assert_eq!(state.recent_subjects[0].subject, "sub-01");
    }
}
