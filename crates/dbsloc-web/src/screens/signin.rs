// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 1 — **Sign in** (`docs/SERVER_CLIENT_PLAN.md` §5.1).
//!
//! v1 has no authentication (decision D2): a base-URL field and *Continue*. No PHI.

use eframe::egui;

use crate::app::WebApp;

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add_space(32.0);
        ui.heading("Wyss DBS Localizer — connect to a GPU server");
        ui.label("Sign in (v1: trusted network, no authentication). Enter the server base URL.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Server URL");
            ui.add(
                egui::TextEdit::singleline(&mut app.base_url_input)
                    .desired_width(380.0)
                    .hint_text("http://127.0.0.1:8000"),
            );
        });
        ui.add_space(8.0);
        if ui.button("Continue").clicked() {
            app.connect();
        }
        ui.separator();
        ui.label(
            "No patient data is stored in the browser; only the server URL and the last dataset \
             label are kept in localStorage.",
        );
        if !app.datasets.is_empty() {
            ui.label(format!("{} dataset(s) visible.", app.datasets.len()));
        }
    });
}
