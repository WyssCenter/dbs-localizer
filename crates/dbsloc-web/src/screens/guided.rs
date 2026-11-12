// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Screen 4 — **Guided steps** (`docs/SERVER_CLIENT_PLAN.md` §5.4).
//!
//! Subject → Preprocess → Coregister → Normalize → Reconstruct → Stimulation → Review, using the shared
//! [`GuidedStep`] / [`StageSettings`] state machines and judging each step against the server's
//! `SubjectSummary` (mapped to [`PipelineReadiness`]) and `plan`.

use eframe::egui;

use dbsloc_api::Stage;
use dbsloc_ui::{
    guided_can_run, guided_refusal, guided_step_check, GuidedStep, MetalContrast, NormResolution,
    ReconMethod, RegistrationEngine, STAGE_META,
};

use crate::app::{Screen, WebApp};

pub(crate) fn show(app: &mut WebApp, ui: &mut egui::Ui) {
    egui::Panel::left("dbsloc_guided_steps")
        .resizable(false)
        .default_size(190.0)
        .show(ui, |ui| {
            ui.heading("Steps");
            for step in GuidedStep::ALL {
                let selected = app.step == step;
                let response = ui.selectable_label(selected, step.title());
                let response = match step.optional_hint() {
                    Some(hint) => response.on_hover_text(hint),
                    None => response,
                };
                if response.clicked() {
                    app.step = step;
                }
            }
            ui.separator();
            if ui.button("Plan (what would run)").clicked() {
                app.request_plan();
            }
            if ui.button("Review screen").clicked() {
                app.screen = Screen::Review;
            }
            if ui.button("Back to subjects").clicked() {
                app.screen = Screen::Landing;
            }
        });

    egui::CentralPanel::default().show(ui, |ui| {
        let step = app.step;
        ui.heading(step.title());
        match step {
            GuidedStep::Subject => subject_card(app, ui),
            GuidedStep::Stimulation => stimulation_card(app, ui),
            GuidedStep::Review => review_card(app, ui),
            _ => {
                if let Some(stage) = step.stage() {
                    stage_card(app, ui, stage);
                }
            }
        }
    });
}

fn subject_card(app: &mut WebApp, ui: &mut egui::Ui) {
    let Some(summary) = app.summary.clone() else {
        ui.label("No subject loaded.");
        return;
    };
    ui.label(format!("Subject: {}", summary.subj_id));
    ui.label(format!("Anchor modality: {}", summary.anchor_modality));
    ui.label(format!("Modalities: {}", summary.modalities.join(", ")));
    ui.separator();
    egui::Grid::new("dbsloc_stage_states")
        .striped(true)
        .show(ui, |ui| {
            for st in &summary.stages {
                ui.label(st.stage.label());
                ui.label(format!("{:?}", st.state));
                ui.label(st.result.clone().unwrap_or_default());
                ui.end_row();
            }
        });
    ui.separator();
    if ui
        .button(format!("Next: {}", GuidedStep::FIRST_WORK.title()))
        .clicked()
    {
        app.step = GuidedStep::FIRST_WORK;
    }
}

fn stimulation_card(app: &mut WebApp, ui: &mut egui::Ui) {
    ui.label(
        egui::RichText::new(
            "Optional. Stimulation settings are edited and computed in the desktop application.",
        )
        .weak(),
    );
    ui.horizontal(|ui| {
        if let Some(prev) = app.step.prev() {
            if ui.button(format!("Back: {}", prev.title())).clicked() {
                app.step = prev;
            }
        }
        if let Some(next) = app.step.next() {
            if ui
                .button("Skip")
                .on_hover_text("Continue without the volume of activated tissue")
                .clicked()
            {
                app.step = next;
            }
        }
    });
}

fn review_card(app: &mut WebApp, ui: &mut egui::Ui) {
    ui.label("Downloads and the (WP5) WebGPU 3D scene live on the review screen.");
    if ui.button("Open review").clicked() {
        app.screen = Screen::Review;
    }
    if let Some(prev) = GuidedStep::Review.prev() {
        if ui.button(format!("Back: {}", prev.title())).clicked() {
            app.step = prev;
        }
    }
}

fn stage_card(app: &mut WebApp, ui: &mut egui::Ui, stage: Stage) {
    if let Some(meta) = STAGE_META.iter().find(|m| m.stage == stage) {
        ui.strong(meta.title);
        ui.label(meta.description);
    }
    ui.separator();
    settings_controls(app, ui, stage);
    ui.separator();

    let (state, text, blockers) = guided_step_check(&app.settings, &app.readiness, stage);
    ui.label(format!("Status: {state:?} — {text}"));
    for b in &blockers {
        ui.colored_label(egui::Color32::YELLOW, format!("• {b}"));
    }

    let can = guided_can_run(&app.settings, &app.readiness, stage);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(can, egui::Button::new(format!("Run {}", stage.label())))
            .clicked()
        {
            app.start_step(stage);
        }
        if !can {
            ui.label(guided_refusal(&app.settings, &app.readiness, stage));
        }
        if let Some(next) = app.step.next() {
            if ui.button(format!("Next: {}", next.title())).clicked() {
                app.step = next;
            }
        }
        if let Some(prev) = app.step.prev() {
            if ui.button(format!("Back: {}", prev.title())).clicked() {
                app.step = prev;
            }
        }
    });

    if let Some(plan) = &app.plan_json {
        ui.separator();
        ui.label("Plan:");
        egui::ScrollArea::vertical()
            .id_salt("dbsloc_plan")
            .max_height(200.0)
            .show(ui, |ui| {
                let text = serde_json::to_string_pretty(plan).unwrap_or_default();
                ui.monospace(text);
            });
    }
}

fn settings_controls(app: &mut WebApp, ui: &mut egui::Ui, stage: Stage) {
    match stage {
        Stage::Preprocess | Stage::Coregister => {
            let engine = if stage == Stage::Preprocess {
                &mut app.settings.preprocess_engine
            } else {
                &mut app.settings.coregister_engine
            };
            ui.horizontal(|ui| {
                ui.label("Engine");
                egui::ComboBox::from_id_salt("dbsloc_engine")
                    .selected_text(engine.label())
                    .show_ui(ui, |ui| {
                        for value in [
                            RegistrationEngine::Auto,
                            RegistrationEngine::Gpu,
                            RegistrationEngine::Ants,
                        ] {
                            ui.selectable_value(&mut *engine, value, value.label());
                        }
                    });
            });
        }
        Stage::Normalize => {
            ui.horizontal(|ui| {
                ui.label("Resolution");
                egui::ComboBox::from_id_salt("dbsloc_res")
                    .selected_text(app.settings.norm_resolution.label())
                    .show_ui(ui, |ui| {
                        for res in [
                            NormResolution::Template,
                            NormResolution::Mm1p0,
                            NormResolution::Mm0p8,
                            NormResolution::Mm0p5,
                            NormResolution::FullTemplate,
                        ] {
                            ui.selectable_value(
                                &mut app.settings.norm_resolution,
                                res,
                                res.label(),
                            );
                        }
                    });
            });
            ui.checkbox(
                &mut app.settings.norm_multispectral,
                "Multispectral (T1w + T2w)",
            );
        }
        Stage::Reconstruct => {
            ui.horizontal(|ui| {
                ui.label("Method");
                egui::ComboBox::from_id_salt("dbsloc_recon")
                    .selected_text(app.settings.recon_method.label())
                    .show_ui(ui, |ui| {
                        for method in [
                            ReconMethod::Auto,
                            ReconMethod::TracCore,
                            ReconMethod::TracCoreNative,
                            ReconMethod::Pacer,
                        ] {
                            ui.selectable_value(
                                &mut app.settings.recon_method,
                                method,
                                method.label(),
                            );
                        }
                    });
            });
            let mut model = app.settings.el_model.clone().unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("Electrode model");
                if ui.text_edit_singleline(&mut model).changed() {
                    app.settings.el_model = if model.trim().is_empty() {
                        None
                    } else {
                        Some(model.clone())
                    };
                }
            });
            ui.horizontal(|ui| {
                ui.label("Metal contrast");
                ui.selectable_value(
                    &mut app.settings.metal_contrast,
                    MetalContrast::Dark,
                    MetalContrast::Dark.label(),
                );
                ui.selectable_value(
                    &mut app.settings.metal_contrast,
                    MetalContrast::Bright,
                    MetalContrast::Bright.label(),
                );
            });
        }
        _ => {}
    }
}
