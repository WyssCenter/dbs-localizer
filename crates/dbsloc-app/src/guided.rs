// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The guided run: one step at a time instead of the whole dock.
//!
//! Each stage step is the ordinary step card of the Pipeline tab, judged as a
//! run of that step alone (`StageSettings::only`), with Back / Run / Next
//! beneath it. Nothing here owns pipeline state: it reads the readiness cache
//! and run progress and hands the app a [`GuidedOutput`] to act on.

use dbsloc_pipeline::stages::Stage;
use eframe::egui;

use crate::app::{DbslocAppState, RunProgress, StageRunStatus};
use crate::panels::{
    card_review, card_to_panel_action, em, palette, progress_bar, registration_shared_options,
    stage_card, stage_options, status_chip, CardAction, CardMode, CardReview, NormalizeBrowse,
    PipelineReadiness, RegistrationReview, ReviewMode, StagePanel, StagePanelAction, StageState,
    REVIEW_REGISTRATIONS_HOVER, REVIEW_REGISTRATIONS_LABEL, STAGE_META,
};
use crate::review::{ReviewOutput, ReviewPanel};

pub use dbsloc_ui::{
    chosen_stimulation, guided_can_run, guided_refusal, guided_step_check, stimulation_computed,
    GuidedStep, NO_STIMULATION_HINT,
};

/// Whether Next may be pressed for a step: it ran in this session, or its
/// output is already on disk.
pub fn guided_next_enabled(status: Option<StageRunStatus>, output_exists: bool) -> bool {
    match status {
        Some(StageRunStatus::AwaitingReview | StageRunStatus::Rejected) => false,
        Some(StageRunStatus::Ok | StageRunStatus::Skipped) => true,
        _ => output_exists,
    }
}

pub fn guided_review_note(status: Option<StageRunStatus>) -> Option<&'static str> {
    match status {
        Some(StageRunStatus::AwaitingReview) => {
            Some("Approve or reject this registration to continue.")
        }
        Some(StageRunStatus::Rejected) => {
            Some("This registration was rejected. Re-run the step to redo it.")
        }
        _ => None,
    }
}

pub fn guided_embeds_review(session_stage: Option<Stage>, step_stage: Option<Stage>) -> bool {
    session_stage.is_some() && session_stage == step_stage
}

pub fn summary_label(title: &str, review: Option<RegistrationReview>) -> String {
    match review {
        Some(state) => format!("{title} · {}", state.chip_text()),
        None => title.to_owned(),
    }
}

/// Whether the step already has a result, on disk or from a run just finished.
pub fn guided_step_done(
    readiness: &PipelineReadiness,
    progress: &RunProgress,
    stage: Stage,
) -> bool {
    match progress.status_of(stage) {
        Some(StageRunStatus::AwaitingReview | StageRunStatus::Rejected) => false,
        Some(StageRunStatus::Ok) => true,
        _ => readiness.stage_output_exists(stage),
    }
}

pub fn guided_step_rerun(
    readiness: &PipelineReadiness,
    progress: &RunProgress,
    stage: Stage,
) -> bool {
    guided_step_done(readiness, progress, stage)
        || matches!(progress.status_of(stage), Some(StageRunStatus::Rejected))
}

/// The action for "run this step": a plain run, or — when the step already has
/// a result — a re-run that asks before replacing it.
pub fn guided_run_action(
    panel: &StagePanel,
    state: &DbslocAppState,
    stage: Stage,
    rerun: bool,
) -> StagePanelAction {
    let Some(mut req) = panel.build_request(state, &[stage]) else {
        return StagePanelAction::None;
    };
    req.open_viewer_after_reconstruct = false;
    if rerun {
        req.overwrite_approved = true;
        StagePanelAction::ConfirmOverwriteThenRun(req, vec![stage])
    } else {
        StagePanelAction::Run(req)
    }
}

pub fn load_vta_surfaces(
    dir: &std::path::Path,
    label: &str,
) -> Vec<dbsloc_viz::vta_mesh::VtaSurface> {
    crate::stimulation_panel::computed_files(dir)
        .into_iter()
        .filter_map(|(side, path)| {
            let mesh = dbsloc_viz::vta_mesh::mesh_vta_nifti(&path).ok()?;
            Some(dbsloc_viz::vta_mesh::VtaSurface {
                side: side.name().to_owned(),
                label: label.to_owned(),
                mesh,
            })
        })
        .collect()
}

pub fn guided_stimulation_action(
    panel: &StagePanel,
    state: &DbslocAppState,
    label: &str,
    rerun: bool,
) -> StagePanelAction {
    let Some(mut req) = panel.build_request(state, &[Stage::Vat]) else {
        return StagePanelAction::None;
    };
    req.open_viewer_after_reconstruct = false;
    req.vat_labels = vec![label.to_owned()];
    req.overwrite_approved = true;
    if rerun {
        StagePanelAction::ConfirmOverwriteThenRun(req, vec![Stage::Vat])
    } else {
        StagePanelAction::Run(req)
    }
}

pub fn guided_step_action(
    panel: &StagePanel,
    state: &DbslocAppState,
    readiness: &PipelineReadiness,
    step: GuidedStep,
    rerun: bool,
) -> StagePanelAction {
    if step == GuidedStep::Stimulation {
        return chosen_stimulation(readiness, state.guided_stimulation.as_deref())
            .map_or(StagePanelAction::None, |label| {
                guided_stimulation_action(panel, state, &label, rerun)
            });
    }
    step.stage().map_or(StagePanelAction::None, |stage| {
        guided_run_action(panel, state, stage, rerun)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuidedNav {
    None,
    Back,
    Next,
    Jump(GuidedStep),
    FullView,
    ChangeSubject,
    ImportImages,
    OpenViewer,
    Finish,
}

pub struct GuidedOutput {
    pub nav: GuidedNav,
    pub action: StagePanelAction,
    pub review: ReviewOutput,
}

/// A filled call-to-action button with the keyboard focus ring drawn explicitly,
/// because the fill suppresses the widget stroke that would otherwise carry it.
pub(crate) fn primary_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    hover: &str,
) -> egui::Response {
    let unit = em(ui);
    primary_button_sized(
        ui,
        label,
        enabled,
        hover,
        egui::vec2(unit * 7.5, unit * 1.9),
    )
}

pub(crate) fn primary_button_sized(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    hover: &str,
    min_size: egui::Vec2,
) -> egui::Response {
    let pal = palette(ui);
    let text = if enabled {
        egui::RichText::new(label)
            .strong()
            .color(egui::Color32::WHITE)
    } else {
        egui::RichText::new(label).strong()
    };
    let mut button = egui::Button::new(text).min_size(min_size);
    if enabled {
        button = button.fill(pal.accent_fill).stroke(egui::Stroke::NONE);
    }
    let response = ui.add_enabled(enabled, button);
    if ui.memory(|m| m.has_focus(response.id)) {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            ui.visuals().widgets.active.corner_radius,
            egui::Stroke::new(2.0_f32, pal.info),
            egui::StrokeKind::Outside,
        );
    }
    response.on_hover_text(hover)
}

pub(crate) fn plain_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    hover: &str,
) -> egui::Response {
    let unit = em(ui);
    ui.add_enabled(
        enabled,
        egui::Button::new(label).min_size(egui::vec2(unit * 5.5, unit * 1.9)),
    )
    .on_hover_text(hover)
}

/// Lay `add` out in a column centred in the free space, at most `max_em` wide.
pub(crate) fn centered_column<R>(
    ui: &mut egui::Ui,
    max_em: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let avail = ui.available_rect_before_wrap();
    let width = avail.width().min(em(ui) * max_em);
    let rect = egui::Rect::from_min_size(
        egui::pos2(avail.center().x - width / 2.0, avail.top()),
        egui::vec2(width, avail.height()),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add)
        .inner
}

const COLUMN_EM: f32 = 40.0;

/// The four steps with a stage, in order, for the summary rows.
const SUMMARY_STAGES: [Stage; 4] = [
    Stage::Preprocess,
    Stage::Coregister,
    Stage::Normalize,
    Stage::Reconstruct,
];

fn summary(ui: &mut egui::Ui, readiness: &PipelineReadiness) {
    let pal = palette(ui);
    for stage in SUMMARY_STAGES {
        let title = STAGE_META
            .iter()
            .find(|m| m.stage == stage)
            .map_or(stage.label(), |m| m.title);
        if readiness.stage_output_exists(stage) {
            let review = readiness.registration_review(stage);
            let glyph = if review == Some(RegistrationReview::Rejected) {
                "⚠"
            } else {
                "✔"
            };
            let colour = if review == Some(RegistrationReview::Rejected) {
                pal.warn
            } else {
                pal.ok
            };
            status_chip(ui, glyph, &summary_label(title, review), colour);
        } else {
            status_chip(ui, "–", title, pal.text_muted);
        }
    }
}

fn stepper_blocker(step: GuidedStep, has_subject: bool, running: bool) -> Option<&'static str> {
    if running {
        Some("Wait for the run to finish")
    } else if step != GuidedStep::Subject && !has_subject {
        Some("Choose a subject first")
    } else {
        None
    }
}

fn stepper(
    ui: &mut egui::Ui,
    step: GuidedStep,
    readiness: &PipelineReadiness,
    running: bool,
    nav: &mut GuidedNav,
) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        for (i, s) in GuidedStep::ALL.iter().enumerate() {
            if i > 0 {
                ui.label(egui::RichText::new("›").color(pal.text_muted));
            }
            let done = match s.stage() {
                Some(Stage::Vat) => !readiness.stimulation_computed.is_empty(),
                Some(stage) => readiness.stage_output_exists(stage),
                None => *s == GuidedStep::Subject && readiness.has_subject,
            };
            let (glyph, colour) = if done {
                ("✔", pal.ok)
            } else if *s == step {
                ("•", pal.text)
            } else {
                ("○", pal.text_muted)
            };
            let label = egui::RichText::new(format!("{glyph} {}", s.title())).color(colour);
            let blocker = stepper_blocker(*s, readiness.has_subject, running);
            let resp = ui
                .add_enabled(
                    blocker.is_none(),
                    egui::Button::selectable(*s == step, label),
                )
                .on_hover_text(s.optional_hint().unwrap_or("Go to this step"))
                .on_disabled_hover_text(blocker.unwrap_or_default());
            if resp.clicked() && *s != step {
                *nav = GuidedNav::Jump(*s);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .link("Switch to full view")
                .on_hover_text("Leave the guide and show every panel")
                .clicked()
            {
                *nav = GuidedNav::FullView;
            }
        });
    });
}

pub struct GuidedInputs<'a> {
    pub step: GuidedStep,
    pub state: &'a mut DbslocAppState,
    pub panel: &'a StagePanel,
    pub readiness: &'a PipelineReadiness,
    pub progress: &'a RunProgress,
    pub review: &'a mut ReviewPanel,
    pub import_block: Option<&'static str>,
}

pub fn show(ui: &mut egui::Ui, inputs: GuidedInputs<'_>) -> GuidedOutput {
    let GuidedInputs {
        step,
        state,
        panel,
        readiness,
        progress,
        review,
        import_block,
    } = inputs;
    let unit = em(ui);
    let pal = palette(ui);
    let mut out = GuidedOutput {
        nav: GuidedNav::None,
        action: StagePanelAction::None,
        review: ReviewOutput::default(),
    };

    ui.add_space(unit * 0.5);
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(unit as i8, 0))
        .show(ui, |ui| {
            stepper(ui, step, readiness, panel.running, &mut out.nav);
        });
    ui.add_space(unit * 0.3);

    let stage = step.stage();
    let check = stage.map(|s| guided_step_check(&state.stage, readiness, s));
    let stimulation = (step == GuidedStep::Stimulation)
        .then(|| chosen_stimulation(readiness, state.guided_stimulation.as_deref()))
        .flatten();
    let can_run = stage.is_some_and(|s| guided_can_run(&state.stage, readiness, s));
    let run_status = stage.and_then(|s| progress.status_of(s));
    let (done, output_exists) = if step == GuidedStep::Stimulation {
        let computed = stimulation_computed(readiness, stimulation.as_deref());
        (computed || run_status == Some(StageRunStatus::Ok), computed)
    } else {
        (
            stage.is_some_and(|s| guided_step_done(readiness, progress, s)),
            stage.is_some_and(|s| readiness.stage_output_exists(s)),
        )
    };
    let rework = run_status == Some(StageRunStatus::Rejected);

    egui::Panel::bottom("guided_actions")
        .frame(
            egui::Frame::NONE
                .fill(pal.surface)
                .inner_margin(egui::Margin::symmetric(unit as i8, (unit * 0.5) as i8)),
        )
        .show_separator_line(true)
        .show(ui, |ui| {
            centered_column(ui, COLUMN_EM, |ui| {
                action_bar(
                    ui,
                    &mut out,
                    &ActionBar {
                        step,
                        can_run,
                        done,
                        rework,
                        run_status,
                        output_exists,
                        running: panel.running,
                        stopping: panel.stopping,
                        has_reconstruction: readiness.has_reconstruction_json,
                        run_wait: panel.run_wait,
                        optional: step.optional_hint().is_some(),
                        compute_label: (step == GuidedStep::Stimulation).then_some("Compute VTA"),
                        run_blocked: if step == GuidedStep::Stimulation && stimulation.is_none() {
                            NO_STIMULATION_HINT
                        } else {
                            "Resolve the item shown above first"
                        },
                    },
                    |rerun| guided_step_action(panel, state, readiness, step, rerun),
                );
            });
        });

    let mut go_to_review: Option<Stage> = None;
    let embed = guided_embeds_review(review.session_stage(), stage);
    let ctx = ui.ctx().clone();

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            let mut stage_section = |ui: &mut egui::Ui| {
                let (Some(stage), Some((status, text, blockers))) = (stage, &check) else {
                    overview(ui, step, state, readiness, import_block, &mut out.nav);
                    return;
                };
                let detail = (stage == Stage::Vat).then(|| match stimulation.as_deref() {
                    Some(label) if stimulation_computed(readiness, Some(label)) => {
                        format!("The VTA of {label} is already computed")
                    }
                    Some(label) => format!("Computes the VTA of {label}"),
                    None => {
                        dbsloc_ui::stage_status_detail(stage, &state.stage.only(stage), readiness)
                            .1
                    }
                });
                let text: &str = detail.as_deref().unwrap_or(text);
                let card_review = card_review(
                    readiness,
                    &state.stage.only(stage),
                    stage,
                    panel.running,
                    panel.review_preparing,
                    panel.refine_activity,
                );
                let (browse, card) = stage_body(
                    ui,
                    stage,
                    *status,
                    text,
                    blockers,
                    run_status,
                    progress.skip_reason(stage),
                    panel.running,
                    &card_review,
                    state,
                    &panel.gpu_help,
                    readiness,
                    stimulation.as_deref(),
                );
                match browse {
                    NormalizeBrowse::Template => {
                        out.action = StagePanelAction::BrowseTemplate;
                    }
                    NormalizeBrowse::TemplateT2 => {
                        out.action = StagePanelAction::BrowseTemplateT2;
                    }
                    NormalizeBrowse::Stimulation => {
                        out.action = StagePanelAction::OpenStimulation;
                    }
                    NormalizeBrowse::None => {}
                }
                if card != CardAction::None {
                    out.action = card_to_panel_action(card, stage);
                }
                if let Some(note) = guided_review_note(run_status) {
                    status_chip(ui, "◔", note, pal.info);
                }
                if panel.running || run_status.is_some() {
                    ui.add_space(unit * 0.3);
                    progress_bar(ui, progress, panel.running);
                }
            };
            let mut open_tab = false;
            if embed {
                egui::Panel::top("guided_review_card")
                    .frame(egui::Frame::NONE)
                    .show_separator_line(false)
                    .show(ui, |ui| {
                        centered_column(ui, COLUMN_EM, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("guided_review_card_body")
                                .max_height(unit * 14.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.add_space(unit * 0.4);
                                    stage_section(ui);
                                });
                            ui.horizontal(|ui| {
                                if ui
                                    .link("Show in Registration review tab")
                                    .on_hover_text(
                                        "Leave the guide and show the registration review in the full view",
                                    )
                                    .clicked()
                                {
                                    open_tab = true;
                                }
                            });
                        });
                    });
                if open_tab {
                    out.action = StagePanelAction::ReviewNow;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(unit as i8, 0)))
                    .show(ui, |ui| {
                        out.review.merge(review.show(ui, &ctx));
                    });
            } else {
                centered_column(ui, COLUMN_EM, |ui| {
                    if let Some(waiting) = review.session_stage() {
                        ui.add_space(unit * 0.4);
                        ui.horizontal_wrapped(|ui| {
                            status_chip(
                                ui,
                                "◔",
                                &format!("A registration review is waiting: {}", waiting.label()),
                                pal.info,
                            );
                            if ui
                                .button("Go to review")
                                .on_hover_text("Open the step that is waiting for your decision")
                                .clicked()
                            {
                                go_to_review = Some(waiting);
                            }
                        });
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("guided_body")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(unit * 0.4);
                            stage_section(ui);
                        });
                });
            }
        });
    if let Some(step) = go_to_review.and_then(|s| {
        GuidedStep::ALL
            .iter()
            .copied()
            .find(|g| g.stage() == Some(s))
    }) {
        out.nav = GuidedNav::Jump(step);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn stage_body(
    ui: &mut egui::Ui,
    stage: Stage,
    status: StageState,
    status_text: &str,
    blockers: &[String],
    run_status: Option<StageRunStatus>,
    skip_reason: Option<&str>,
    running: bool,
    review: &CardReview,
    state: &mut DbslocAppState,
    gpu_help: &crate::gpu_status::GpuHelp,
    readiness: &PipelineReadiness,
    stimulation: Option<&str>,
) -> (NormalizeBrowse, CardAction) {
    let pal = palette(ui);
    let Some(meta) = STAGE_META.iter().find(|m| m.stage == stage) else {
        return (NormalizeBrowse::None, CardAction::None);
    };
    let mut browse = NormalizeBrowse::None;
    let mut selected = true;
    let open_options = matches!(stage, Stage::Normalize | Stage::Reconstruct)
        || (stage == Stage::Coregister && crate::app::has_both_postop(readiness));
    let mode = CardMode::Guided { open_options };
    let card = stage_card(
        ui,
        meta,
        status,
        status_text,
        run_status,
        skip_reason,
        !running,
        false,
        review,
        &pal,
        &mut selected,
        mode,
        |ui, part| {
            if part == crate::panels::CardPart::Body {
                if stage == Stage::Vat {
                    stimulation_choice(ui, state, readiness, stimulation);
                    let wanted = crate::panels::vat_card_body(ui, state, readiness, &pal);
                    if wanted != NormalizeBrowse::None {
                        browse = wanted;
                    }
                }
                return;
            }
            browse = stage_options(stage, ui, state, &pal, mode, readiness);
            if matches!(
                stage,
                Stage::Preprocess | Stage::Coregister | Stage::Normalize
            ) {
                ui.add_space(em(ui) * 0.3);
                registration_shared_options(ui, state, gpu_help);
            }
            if matches!(stage, Stage::Coregister | Stage::Normalize) {
                let mut review = state.stage.review_mode != ReviewMode::Off;
                if ui
                    .checkbox(&mut review, REVIEW_REGISTRATIONS_LABEL)
                    .on_hover_text(REVIEW_REGISTRATIONS_HOVER)
                    .changed()
                {
                    state.stage.review_mode = if review {
                        ReviewMode::DuringRun
                    } else {
                        ReviewMode::Off
                    };
                }
            }
        },
    );
    if status == StageState::Ready {
        for blocker in blockers {
            status_chip(ui, "⚠", blocker, pal.warn);
        }
    }
    (browse, card)
}

fn stimulation_choice(
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    readiness: &PipelineReadiness,
    current: Option<&str>,
) {
    if readiness.stimulation_labels.len() < 2 {
        return;
    }
    let mut chosen = current.unwrap_or_default().to_owned();
    ui.horizontal(|ui| {
        ui.label("Stimulation");
        egui::ComboBox::from_id_salt("guided_stimulation")
            .selected_text(chosen.as_str())
            .show_ui(ui, |ui| {
                for label in &readiness.stimulation_labels {
                    ui.selectable_value(&mut chosen, label.clone(), label);
                }
            })
            .response
            .on_hover_text("The saved stimulation whose volume of activated tissue is computed");
    });
    if current != Some(chosen.as_str()) {
        state.guided_stimulation = Some(chosen);
    }
}

/// The Subject and Review pages: who this is, and what already exists.
fn overview(
    ui: &mut egui::Ui,
    step: GuidedStep,
    state: &DbslocAppState,
    readiness: &PipelineReadiness,
    import_block: Option<&'static str>,
    nav: &mut GuidedNav,
) {
    let pal = palette(ui);
    let unit = em(ui);
    egui::Frame::group(ui.style())
        .fill(pal.surface_raised)
        .stroke(egui::Stroke::new(1.0_f32, pal.hairline))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let subject = state.selected_subject.as_deref().unwrap_or("No subject");
            ui.add(
                egui::Label::new(egui::RichText::new(subject).heading().strong()).selectable(false),
            );
            let folder = std::path::Path::new(&state.bids_dir)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.add(
                egui::Label::new(egui::RichText::new(folder).small().color(pal.text_muted))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(&state.bids_dir);
            ui.add_space(unit * 0.4);
            summary(ui, readiness);
            if step == GuidedStep::Subject {
                ui.add_space(unit * 0.8);
                if ui
                    .button("Change subject")
                    .on_hover_text("Return to the start screen")
                    .clicked()
                {
                    *nav = GuidedNav::ChangeSubject;
                }
                if ui
                    .add_enabled(
                        import_block.is_none(),
                        egui::Button::new("Import raw NIfTI / DICOM…"),
                    )
                    .on_hover_text("Create a subject from NIfTI files or DICOM folders (Ctrl+I)")
                    .on_disabled_hover_text(import_block.unwrap_or_default())
                    .clicked()
                {
                    *nav = GuidedNav::ImportImages;
                }
            }
        });
}

struct ActionBar {
    step: GuidedStep,
    can_run: bool,
    done: bool,
    rework: bool,
    run_status: Option<StageRunStatus>,
    output_exists: bool,
    running: bool,
    stopping: bool,
    has_reconstruction: bool,
    run_wait: Option<&'static str>,
    optional: bool,
    compute_label: Option<&'static str>,
    run_blocked: &'static str,
}

fn action_bar(
    ui: &mut egui::Ui,
    out: &mut GuidedOutput,
    bar: &ActionBar,
    run_action: impl FnOnce(bool) -> StagePanelAction,
) {
    ui.horizontal(|ui| {
        if plain_button(
            ui,
            "Back",
            bar.step.prev().is_some() && !bar.running,
            "Previous step",
        )
        .clicked()
        {
            out.nav = GuidedNav::Back;
        }
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match bar.step {
                GuidedStep::Subject => {
                    if primary_button(ui, "Next", true, "Start with the first step").clicked() {
                        out.nav = GuidedNav::Next;
                    }
                }
                GuidedStep::Review => {
                    if primary_button(
                        ui,
                        "Open 3D viewer",
                        bar.has_reconstruction && !bar.running,
                        if bar.has_reconstruction {
                            "Show the electrode in the 3D viewer"
                        } else {
                            "Reconstruct first"
                        },
                    )
                    .clicked()
                    {
                        out.nav = GuidedNav::OpenViewer;
                    }
                    if plain_button(
                        ui,
                        "Finish",
                        !bar.running,
                        "Leave the guide, keep the results",
                    )
                    .clicked()
                    {
                        out.nav = GuidedNav::Finish;
                    }
                }
                _ => stage_buttons(ui, out, bar, run_action),
            },
        );
    });
}

fn stage_buttons(
    ui: &mut egui::Ui,
    out: &mut GuidedOutput,
    bar: &ActionBar,
    run_action: impl FnOnce(bool) -> StagePanelAction,
) {
    let next_ok = !bar.running
        && (bar.optional || guided_next_enabled(bar.run_status, bar.output_exists) || !bar.can_run);
    let next_label = if matches!(
        bar.run_status,
        Some(StageRunStatus::Ok | StageRunStatus::Skipped)
    ) {
        "Next"
    } else if bar.output_exists {
        "Use existing"
    } else if !bar.can_run || bar.optional {
        "Skip"
    } else {
        "Next"
    };
    let next_hover = match next_label {
        "Use existing" => "Keep the result already on disk and continue",
        "Skip" if bar.optional => "Continue without computing the volume of activated tissue",
        "Skip" => "This step cannot run here; continue without it",
        _ => "Continue to the next step",
    };
    let run_label = if bar.running {
        "Running…"
    } else if bar.done || bar.rework {
        "Re-run"
    } else {
        bar.compute_label.unwrap_or("Run this step")
    };
    let run_enabled = bar.can_run && !bar.running && bar.run_wait.is_none();
    let run_hover = if let Some(wait) = bar.run_wait {
        wait
    } else if bar.can_run {
        if bar.compute_label.is_some() {
            "Compute the volume of activated tissue of the chosen stimulation (Ctrl+R)"
        } else {
            "Run only this step (Ctrl+R)"
        }
    } else {
        bar.run_blocked
    };

    let next_primary = bar.done || !bar.can_run;
    let (next_clicked, run_clicked) = if next_primary {
        (
            primary_button(ui, next_label, next_ok, next_hover).clicked(),
            plain_button(ui, run_label, run_enabled, run_hover).clicked(),
        )
    } else {
        (
            plain_button(ui, next_label, next_ok, next_hover).clicked(),
            primary_button(ui, run_label, run_enabled, run_hover).clicked(),
        )
    };
    if bar.running
        && plain_button(
            ui,
            if bar.stopping { "Stopping…" } else { "Stop" },
            !bar.stopping,
            crate::panels::STOP_HOVER,
        )
        .clicked()
    {
        out.action = StagePanelAction::Cancel;
    }
    if next_clicked {
        out.nav = GuidedNav::Next;
    } else if run_clicked {
        out.action = run_action(bar.done || bar.rework);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::{MetalContrast, ReconMethod, StageSettings};
    use dbsloc_pipeline::protocol::RunEvent;

    fn ready() -> PipelineReadiness {
        PipelineReadiness {
            has_bids_dir: true,
            has_subject: true,
            template_exists: true,
            has_raw_anat: true,
            has_raw_postop_ct: true,
            has_ants_registration: true,
            has_n4_bias_correction: true,
            gpu_available: Some(true),
            ..PipelineReadiness::default()
        }
    }

    #[test]
    fn stepper_allows_forward_jumps_once_a_subject_is_chosen() {
        for step in GuidedStep::ALL {
            assert_eq!(stepper_blocker(step, true, false), None);
            assert_eq!(
                stepper_blocker(step, true, true),
                Some("Wait for the run to finish")
            );
        }
        assert_eq!(stepper_blocker(GuidedStep::Subject, false, false), None);
        assert_eq!(
            stepper_blocker(GuidedStep::Preprocess, false, false),
            Some("Choose a subject first")
        );
    }

    #[test]
    fn steps_run_in_pipeline_order_and_stop_at_both_ends() {
        let order = [
            GuidedStep::Subject,
            GuidedStep::Preprocess,
            GuidedStep::Coregister,
            GuidedStep::Normalize,
            GuidedStep::Reconstruct,
            GuidedStep::Stimulation,
            GuidedStep::Review,
        ];
        assert_eq!(GuidedStep::ALL, order);
        for pair in order.windows(2) {
            assert_eq!(pair[0].next(), Some(pair[1]));
            assert_eq!(pair[1].prev(), Some(pair[0]));
        }
        for step in order {
            assert_eq!(
                GuidedStep::from_name(&step.title().to_lowercase()),
                Some(step)
            );
        }
        assert_eq!(GuidedStep::from_name("brainshift"), None);
        assert_eq!(GuidedStep::Subject.prev(), None);
        assert_eq!(GuidedStep::Review.next(), None);
    }

    #[test]
    fn only_the_five_working_steps_have_a_stage() {
        let stages: Vec<Option<Stage>> = GuidedStep::ALL.iter().map(|s| s.stage()).collect();
        assert_eq!(
            stages,
            vec![
                None,
                Some(Stage::Preprocess),
                Some(Stage::Coregister),
                Some(Stage::Normalize),
                Some(Stage::Reconstruct),
                Some(Stage::Vat),
                None,
            ]
        );
        assert!(stages
            .iter()
            .flatten()
            .all(|s| Stage::work_sequence().contains(s)));
        assert_eq!(GuidedStep::FIRST_WORK.stage(), Some(Stage::Preprocess));
    }

    #[test]
    fn next_needs_a_finished_step_or_an_existing_output() {
        use StageRunStatus::*;
        for (status, exists, expected) in [
            (None, false, false),
            (None, true, true),
            (Some(Pending), false, false),
            (Some(Running), false, false),
            (Some(Ok), false, true),
            (Some(Skipped), false, true),
            (Some(Cancelled), false, false),
            (Some(Failed), false, false),
            (Some(Failed), true, true),
            (Some(Ok), true, true),
            (Some(AwaitingReview), false, false),
            (Some(AwaitingReview), true, false),
            (Some(Rejected), false, false),
            (Some(Rejected), true, false),
        ] {
            assert_eq!(
                guided_next_enabled(status, exists),
                expected,
                "{status:?} output_exists={exists}"
            );
        }
    }

    #[test]
    fn a_single_step_run_does_not_count_an_unselected_upstream_step() {
        let settings = StageSettings {
            el_model: Some("any".to_owned()),
            ..StageSettings::default()
        };
        let readiness = ready();
        assert!(!guided_can_run(&settings, &readiness, Stage::Normalize));
        assert!(!guided_can_run(&settings, &readiness, Stage::Reconstruct));
        let coregistered = PipelineReadiness {
            has_coreg_warped: true,
            ..ready()
        };
        assert!(guided_can_run(&settings, &coregistered, Stage::Normalize));
        let normalized = PipelineReadiness {
            has_norm_postop_ct: true,
            has_norm_nifti: true,
            ..readiness
        };
        assert!(guided_can_run(&settings, &normalized, Stage::Reconstruct));
    }

    #[test]
    fn reconstruct_waits_for_an_electrode_model() {
        let readiness = PipelineReadiness {
            has_norm_postop_ct: true,
            has_norm_nifti: true,
            ..ready()
        };
        let mut settings = StageSettings::default();
        assert!(!guided_can_run(&settings, &readiness, Stage::Reconstruct));
        assert!(guided_refusal(&settings, &readiness, Stage::Reconstruct).contains("electrode"));
        settings.el_model = Some("any".to_owned());
        assert!(guided_can_run(&settings, &readiness, Stage::Reconstruct));
    }

    #[test]
    fn a_step_without_its_input_cannot_run() {
        let readiness = PipelineReadiness {
            has_raw_postop_ct: false,
            ..ready()
        };
        let settings = StageSettings::default();
        assert!(!guided_can_run(&settings, &readiness, Stage::Coregister));
        assert_eq!(
            guided_refusal(&settings, &readiness, Stage::Coregister),
            "Coregistration needs a post-operative CT or MRI in this subject."
        );
    }

    #[test]
    fn a_single_stage_request_carries_one_stage_and_the_settings() {
        let mut state = DbslocAppState::default();
        state.stage.recon_method = ReconMethod::Pacer;
        state.stage.metal_contrast = MetalContrast::Bright;
        state.stage.el_model = Some("any".to_owned());
        let panel = StagePanel::default();

        let StagePanelAction::Run(req) =
            guided_run_action(&panel, &state, Stage::Reconstruct, false)
        else {
            panic!("expected a plain run");
        };
        assert_eq!(req.stages, vec![Stage::Reconstruct]);
        assert!(!req.overwrite_approved);
        assert!(!req.open_viewer_after_reconstruct);
        assert_eq!(req.recon_method, ReconMethod::Pacer);
        assert!(req.recon_bright_metal);
        assert_eq!(req.el_model.as_deref(), Some("any"));
    }

    #[test]
    fn the_stimulation_step_runs_only_the_chosen_label() {
        let state = DbslocAppState::default();
        let panel = StagePanel::default();
        let StagePanelAction::Run(req) = guided_stimulation_action(&panel, &state, "demo", false)
        else {
            panic!("expected a plain run");
        };
        assert_eq!(req.stages, vec![Stage::Vat]);
        assert_eq!(req.vat_labels, vec!["demo".to_owned()]);
        let StagePanelAction::ConfirmOverwriteThenRun(req, stages) =
            guided_stimulation_action(&panel, &state, "demo", true)
        else {
            panic!("expected a confirmation");
        };
        assert_eq!(req.vat_labels, vec!["demo".to_owned()]);
        assert_eq!(stages, vec![Stage::Vat]);
        let none = PipelineReadiness::default();
        assert!(matches!(
            guided_step_action(&panel, &state, &none, GuidedStep::Stimulation, false),
            StagePanelAction::None
        ));
    }

    #[test]
    fn a_rerun_asks_before_it_replaces_the_result() {
        let state = DbslocAppState::default();
        let panel = StagePanel::default();
        let StagePanelAction::ConfirmOverwriteThenRun(req, stages) =
            guided_run_action(&panel, &state, Stage::Normalize, true)
        else {
            panic!("expected a confirmation");
        };
        assert_eq!(req.stages, vec![Stage::Normalize]);
        assert!(req.overwrite_approved);
        assert_eq!(stages, vec![Stage::Normalize]);
    }

    #[test]
    fn a_finished_run_counts_as_done_before_the_rescan() {
        let readiness = ready();
        let mut progress = RunProgress::default();
        assert!(!guided_step_done(&readiness, &progress, Stage::Preprocess));
        let with_output = PipelineReadiness {
            has_preprocess_nifti: true,
            ..ready()
        };
        assert!(guided_step_done(&with_output, &progress, Stage::Preprocess));
        progress.start(&[Stage::Preprocess]);
        progress.on_event(&RunEvent::StageCompleted {
            stage: Stage::Preprocess.label().to_owned(),
            result: None,
        });
        assert!(guided_step_done(&readiness, &progress, Stage::Preprocess));
    }

    #[test]
    fn a_step_in_review_is_never_done_and_a_rejected_one_reruns() {
        let readiness = PipelineReadiness {
            has_coreg_warped: true,
            ..ready()
        };
        let mut progress = RunProgress::default();
        progress.start(&[Stage::Coregister]);
        assert!(guided_step_done(&readiness, &progress, Stage::Coregister));
        progress.on_review_requested(Stage::Coregister);
        assert!(!guided_step_done(&readiness, &progress, Stage::Coregister));
        assert!(!guided_step_rerun(&readiness, &progress, Stage::Coregister));
        progress.on_event(&RunEvent::StageAwaitingQc {
            stage: Stage::Coregister.label().to_owned(),
        });
        assert!(!guided_step_done(&readiness, &progress, Stage::Coregister));
        assert!(guided_step_rerun(&readiness, &progress, Stage::Coregister));
    }

    #[test]
    fn the_review_note_follows_the_run_status() {
        use StageRunStatus::*;
        assert_eq!(
            guided_review_note(Some(AwaitingReview)),
            Some("Approve or reject this registration to continue.")
        );
        assert_eq!(
            guided_review_note(Some(Rejected)),
            Some("This registration was rejected. Re-run the step to redo it.")
        );
        for status in [None, Some(Pending), Some(Running), Some(Ok), Some(Failed)] {
            assert_eq!(guided_review_note(status), None, "{status:?}");
        }
    }

    #[test]
    fn the_review_panel_is_embedded_only_for_a_step_with_a_session() {
        assert!(guided_embeds_review(
            Some(Stage::Coregister),
            Some(Stage::Coregister)
        ));
        assert!(!guided_embeds_review(None, Some(Stage::Coregister)));
        assert!(!guided_embeds_review(Some(Stage::Coregister), None));
        assert!(!guided_embeds_review(
            Some(Stage::Coregister),
            Some(Stage::Normalize)
        ));
    }

    #[test]
    fn summary_rows_name_the_review_state() {
        assert_eq!(summary_label("Normalize", None), "Normalize");
        assert_eq!(
            summary_label("Normalize", Some(RegistrationReview::Approved)),
            "Normalize · Reviewed ✔"
        );
        assert_eq!(
            summary_label("Coregister", Some(RegistrationReview::Rejected)),
            "Coregister · Rejected"
        );
        assert_eq!(
            summary_label("Coregister", Some(RegistrationReview::NotReviewed)),
            "Coregister · Not reviewed"
        );
    }
}
