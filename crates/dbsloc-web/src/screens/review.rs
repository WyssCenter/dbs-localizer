// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 7 — **Review** (`docs/SERVER_CLIENT_PLAN.md` §5.7).
//!
//! Downloads and the byte loaders of the `dbsloc-viz` `web` feature. The interactive WebGPU 3D
//! scene is WP5; here the client fetches the server's scene manifest and the referenced
//! template NIfTI / Reco v2 JSON and runs [`TemplateVolume::from_bytes`] and
//! [`load_mni_side_geometry_from_bytes`] on them, so the loaders are wired and exercised without
//! faking the viewer.
//!
//! [`TemplateVolume::from_bytes`]: dbsloc_viz::scene_volume::TemplateVolume::from_bytes
//! [`load_mni_side_geometry_from_bytes`]: dbsloc_viz::reco_json::load_mni_side_geometry_from_bytes

use eframe::egui;

use crate::app::{Screen, WebApp};

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading("Review");
        ui.horizontal(|ui| {
            if ui.button("Load scene (byte loaders)").clicked() {
                app.load_scene();
            }
            if ui.button("Back to steps").clicked() {
                app.screen = Screen::Guided;
            }
        });

        ui.separator();
        ui.strong("Byte loaders (dbsloc-viz web)");
        ui.label(format!(
            "Template volume: {}",
            app.review
                .template_dims
                .map(|(a, b, c)| format!("{a}×{b}×{c}"))
                .unwrap_or_else(|| "not loaded".to_owned())
        ));
        ui.label(format!(
            "Precomputed AHEAD atlas meshes available: {}",
            dbsloc_viz::web_atlas::AHEAD_PRECOMPUTED_OBJECTS.len()
        ));
        if app.review.reco_sides.is_empty() {
            ui.label("Reco trajectory: not loaded");
        }
        for (side, geom) in &app.review.reco_sides {
            ui.label(format!(
                "Reco {side} trajectory: {} point(s)",
                geom.trajectory.len()
            ));
        }
        egui::ScrollArea::vertical()
            .id_salt("dbsloc_review_notes")
            .max_height(120.0)
            .show(ui, |ui| {
                for note in &app.review.notes {
                    ui.monospace(note.clone());
                }
            });

        ui.separator();
        ui.strong("3D scene");
        ui.colored_label(
            egui::Color32::GRAY,
            "The interactive WebGPU 3D scene renders in WP5. The byte loaders above are wired and \
             exercised against the server's files.",
        );

        ui.separator();
        ui.strong("Downloads");
        let base = app.api.base().trim_end_matches('/').to_owned();
        let dataset = app.selected_dataset.clone();
        let subject = app.selected_subject.clone();
        if app.review.downloads.is_empty() {
            ui.label("No download keys yet — open a subject and run a stage.");
        }
        for key in app.review.downloads.clone() {
            match (&dataset, &subject) {
                (Some(ds), Some(sid)) => {
                    let url = crate::api::file_endpoint(&base, ds, sid, &key);
                    ui.add(
                        egui::Hyperlink::from_label_and_url(key.clone(), url).open_in_new_tab(true),
                    );
                }
                _ => {
                    ui.label(key.clone());
                }
            }
        }
    });
}
