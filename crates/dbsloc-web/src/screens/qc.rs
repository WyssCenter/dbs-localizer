// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 6 — **QC** (`docs/SERVER_CLIENT_PLAN.md` §5.6, §4.3).
//!
//! v1 shows the checkreg **PNG** slides with *Approve* / *Reject* per slide, posting to the
//! approval API. The interactive volume review (window / level, slice scroll, overlay) is the
//! WebGPU viewer in WP5; the shared [`CheckregReviewState`] drives slide navigation here.
//!
//! [`CheckregReviewState`]: dbsloc_viz::checkreg_view::CheckregReviewState

use eframe::egui;

use dbsloc_viz::checkreg_view::ReviewAction;

use crate::app::{Screen, WebApp};

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.heading("Quality control — checkreg slides");

        if app.qc.manifest.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading the checkreg manifest…");
            });
            if ui.button("Retry").clicked() {
                app.request_qc();
            }
            if ui.button("Back to progress").clicked() {
                app.screen = Screen::Progress;
            }
            return;
        }

        let (index, count) = app
            .qc
            .review
            .as_ref()
            .map(|r| (r.slide_index(), r.slide_count()))
            .unwrap_or((0, 0));
        let current_key = app
            .qc
            .manifest
            .as_ref()
            .and_then(|m| m.slides.get(index))
            .map(|s| s.checkreg_png.clone());
        let current_approved = app
            .qc
            .manifest
            .as_ref()
            .and_then(|m| m.slides.get(index))
            .map(|s| s.approved)
            .unwrap_or(false);

        ui.label(format!("Slide {} of {}", index + 1, count));
        match current_key.clone() {
            Some(key) if key.is_empty() => {
                ui.colored_label(egui::Color32::YELLOW, "Slide image unavailable.");
            }
            Some(key) => slide_image(app, ui, &key),
            None => {}
        }

        ui.horizontal(|ui| {
            if current_approved {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Approved");
            }
            if ui.button("Approve").clicked() {
                let decision = app
                    .qc
                    .review
                    .as_ref()
                    .map(|r| r.decide(ReviewAction::Approve));
                if let Some(d) = decision {
                    post_approval(app, d.slide as u32, true);
                }
            }
            if ui.button("Reject").clicked() {
                let decision = app
                    .qc
                    .review
                    .as_ref()
                    .map(|r| r.decide(ReviewAction::Reject));
                if let Some(d) = decision {
                    post_approval(app, d.slide as u32, false);
                }
            }
            if ui.button("Next slide").clicked() {
                if let Some(r) = app.qc.review.as_mut() {
                    let _ = r.goto_next_slide();
                }
            }
            if ui.button("Back to progress").clicked() {
                app.screen = Screen::Progress;
            }
        });
    });
}

fn post_approval(app: &mut WebApp, position: u32, ok: bool) {
    let slide = app
        .qc
        .manifest
        .as_ref()
        .and_then(|m| m.slides.get(position as usize))
        .map_or(position, |s| s.index);
    if let (Some(ds), Some(sid)) = (app.selected_dataset.clone(), app.selected_subject.clone()) {
        app.api.post_approval(&ds, &sid, slide, ok);
    }
}

fn slide_image(app: &mut WebApp, ui: &mut egui::Ui, key: &str) {
    if let Some(texture) = app.textures.get(key) {
        let size = texture.size_vec2();
        let avail = ui.available_size();
        let scale = (avail.x / size.x).min(avail.y / size.y).min(1.0);
        ui.add(
            egui::Image::new(egui::load::SizedTexture::from(texture))
                .fit_to_exact_size(size * scale),
        );
    } else if let Some(err) = app.qc.failed.get(key).cloned() {
        ui.horizontal(|ui| {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                format!("Slide image failed: {err}"),
            );
            if ui.button("Retry").clicked() {
                app.retry_slide(key);
            }
        });
    } else {
        if !app.qc.requested.contains(key) {
            app.request_slide(key);
        }
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Loading slide image…");
        });
    }
}
