// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 3 — **Upload** (`docs/SERVER_CLIENT_PLAN.md` §5.3, §4.2).
//!
//! Pick NIfTI or DICOM files in the browser, check them (a streamed SHA-256) and upload them in
//! chunks with a progress bar. DICOM files go through the server's `convert` endpoint
//! (decision D5) and the converted series are listed before the commit; the BIDS mapping is
//! editable and committed together with the subject id.

use eframe::egui;

use crate::app::{Screen, UploadPhase, WebApp};

const SESSIONS: [(&str, &str); 2] = [("preop", "Pre-op"), ("postop", "Post-op")];
const ACQS: [(&str, &str); 4] = [
    ("ax", "Axial"),
    ("cor", "Coronal"),
    ("sag", "Sagittal"),
    ("iso", "Isotropic"),
];

fn choice(
    ui: &mut egui::Ui,
    id: (&str, usize),
    value: &mut Option<String>,
    options: &[(&str, &str)],
) {
    let shown = options
        .iter()
        .find(|(v, _)| Some(*v) == value.as_deref())
        .map_or("not set", |(_, label)| *label);
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "not set");
            for (v, label) in options {
                ui.selectable_value(value, Some((*v).to_owned()), *label);
            }
        });
}

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading("Upload a subject");
        ui.label(
            "Pick NIfTI or DICOM files; they upload in chunks and are committed with a proposed \
             BIDS mapping.",
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!app.upload.busy(), egui::Button::new("Choose files…"))
                .clicked()
            {
                app.pick_files();
            }
            if ui.button("Back").clicked() {
                app.screen = Screen::Landing;
            }
        });

        if !app.upload.message.is_empty() {
            ui.label(&app.upload.message);
        }
        if app.upload.files.is_empty() {
            ui.separator();
            ui.label("No files chosen yet.");
            return;
        }

        ui.separator();
        ui.label(format!(
            "{} file(s), {} bytes total",
            app.upload.files.len(),
            app.upload.total_bytes
        ));
        egui::Grid::new("dbsloc_upload_files")
            .striped(true)
            .show(ui, |ui| {
                for f in &app.upload.files {
                    ui.label(f.name.as_str());
                    ui.label(format!("{} bytes", f.size));
                    ui.end_row();
                }
            });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Subject id");
            ui.text_edit_singleline(&mut app.upload.subject);
        });
        ui.horizontal(|ui| {
            let can_start = !app.upload.busy() && app.upload.phase != UploadPhase::Ready;
            if ui
                .add_enabled(can_start, egui::Button::new("Upload chunks"))
                .clicked()
            {
                app.begin_upload();
            }
            ui.add(
                egui::ProgressBar::new(app.upload.progress())
                    .show_percentage()
                    .text(app.upload.progress_label()),
            );
        });

        if !app.upload.series.is_empty() {
            ui.separator();
            ui.label("DICOM series found");
            egui::Grid::new("dbsloc_upload_series")
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("file");
                    ui.strong("description");
                    ui.strong("modality");
                    ui.strong("size (voxels)");
                    ui.end_row();
                    for s in &app.upload.series {
                        ui.label(s.file.as_str());
                        ui.label(s.description.as_str());
                        ui.label(s.modality.as_str());
                        ui.label(format!("{} × {} × {}", s.dims[0], s.dims[1], s.dims[2]));
                        ui.end_row();
                    }
                });
        }

        ui.separator();
        ui.label("Proposed BIDS mapping (editable)");
        egui::Grid::new("dbsloc_upload_mapping")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("file");
                ui.strong("session");
                ui.strong("suffix");
                ui.strong("orientation");
                ui.end_row();
                for (i, entry) in app.upload.mapping.iter_mut().enumerate() {
                    ui.label(entry.file.as_str());
                    choice(ui, ("session", i), &mut entry.session, &SESSIONS);
                    ui.text_edit_singleline(&mut entry.suffix);
                    choice(ui, ("acq", i), &mut entry.acq, &ACQS);
                    ui.end_row();
                }
            });
        ui.checkbox(
            &mut app.upload.replace_existing,
            "Replace images that already exist for this subject",
        );
        let can_commit = app.upload.phase == UploadPhase::Ready
            && !app.upload.mapping.is_empty()
            && !app.upload.busy();
        if ui
            .add_enabled(can_commit, egui::Button::new("Commit to BIDS"))
            .clicked()
        {
            app.commit_upload();
        }
    });
}
