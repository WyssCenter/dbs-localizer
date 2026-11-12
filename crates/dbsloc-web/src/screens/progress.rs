// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 5 — **Progress** (`docs/SERVER_CLIENT_PLAN.md` §5.5).
//!
//! The SSE log, k-of-N stage counts, elapsed time and *Cancel*. Reconnect replays the run's
//! `.ndjson` through the `EventSource` (see [`crate::sse`]).

use eframe::egui;

use dbsloc_api::RunPhase;

use crate::app::{Screen, WebApp};
use crate::track::is_active;

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading("Run progress");

        if let Some(run) = app.run.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("phase: {:?}", run.phase));
                ui.separator();
                ui.label(format!(
                    "stage: {}",
                    run.stage.clone().unwrap_or_else(|| "—".to_owned())
                ));
                ui.separator();
                ui.label(format!(
                    "ok {} / failed {} / skipped {}",
                    run.stages_ok, run.stages_failed, run.stages_skipped
                ));
                if let Some(ms) = run.elapsed_ms {
                    ui.separator();
                    ui.label(format!("{:.1} s", ms as f64 / 1000.0));
                }
            });

            ui.horizontal(|ui| {
                if is_active(run.phase) && ui.button("Cancel").clicked() {
                    if let Some(id) = app.run_id.clone() {
                        app.api.cancel_run(&id);
                    }
                }
                if run.phase == RunPhase::AwaitingQc && ui.button("Open QC review").clicked() {
                    app.screen = Screen::Qc;
                }
                if run.phase == RunPhase::Done && ui.button("Open review").clicked() {
                    app.screen = Screen::Review;
                }
                if ui.button("Back to steps").clicked() {
                    app.screen = Screen::Guided;
                }
            });

            match run.phase {
                RunPhase::Failed => {
                    ui.colored_label(egui::Color32::LIGHT_RED, "The run failed.");
                }
                RunPhase::Cancelled => {
                    ui.colored_label(egui::Color32::YELLOW, "The run was cancelled.");
                }
                RunPhase::Rejected => {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "A slide was rejected; the run stopped at its quality-control gate.",
                    );
                }
                RunPhase::Resumed => {
                    ui.label("Quality control approved; the run continues under a new run.");
                }
                _ => {}
            }
            if let Some(msg) = &run.message {
                ui.colored_label(egui::Color32::LIGHT_RED, msg);
            }
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Waiting for the run status…");
            });
        }

        ui.separator();
        ui.label("Log");
        egui::ScrollArea::vertical()
            .id_salt("dbsloc_run_log")
            .stick_to_bottom(true)
            .max_height(420.0)
            .show(ui, |ui| {
                for line in &app.log {
                    ui.monospace(line.clone());
                }
            });
    });
}
