// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 2 — **Landing** (`docs/SERVER_CLIENT_PLAN.md` §5.2).
//!
//! Datasets the caller may see, then the subject list of the chosen dataset, plus *Upload*.

use eframe::egui;

use crate::app::{Screen, WebApp};

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading("Datasets");
        if app.datasets.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading datasets…");
                if ui.button("Retry").clicked() {
                    app.api.get_datasets();
                }
            });
        }
        for ds in app.datasets.clone() {
            let selected = app.selected_dataset.as_deref() == Some(ds.name.as_str());
            let label = format!("{}  ({} subjects)", ds.name, ds.subjects);
            if ui.selectable_label(selected, label).clicked() {
                app.select_dataset(&ds.name);
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Upload subject…").clicked() {
                app.screen = Screen::Upload;
            }
            if ui.button("Refresh datasets").clicked() {
                app.api.get_datasets();
            }
        });

        ui.separator();
        if let Some(ds) = app.selected_dataset.clone() {
            ui.heading(format!("Subjects in {ds}"));
            if app.subjects.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading subjects…");
                });
            }
            for sid in app.subjects.clone() {
                if ui.selectable_label(false, sid.clone()).clicked() {
                    app.open_subject(&sid);
                }
            }
        } else {
            ui.label("Choose a dataset to list its subjects.");
        }
    });
}
