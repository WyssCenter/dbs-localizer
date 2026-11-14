// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Panel implementations for the dockable areas of the pipeline GUI.
//!
//! Every panel here is *pure UI*: it renders from state the caller owns and
//! reports back what the user asked for. Nothing in this module touches the
//! filesystem on the paint path — directory scans run on worker threads and are
//! cached (see [`ReadinessCache`] and [`SubjectPanel`]), so a frame never blocks
//! on I/O.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use dbsloc_pipeline::options::NormGrid;
use dbsloc_pipeline::reg_backend::RegBackend;
use dbsloc_pipeline::ResolvedNormGrid;
use dbsloc_pipeline::{protocol::RunEvent, stages::Stage};
use dbsloc_viz::{
    BackdropChoice, BackdropRequest, NavigationScheme, Scene3DEframeApp, Scene3DPayload,
    SceneManifestV1, SceneViewState, VolumePreview,
};
use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::backdrop_download::{
    backdrop_choices, custom_id, BackdropDownloadUi, Outcome, CUSTOM_PREFIX,
};
use dbsloc_pipeline::backdrops::{BackdropKind, BackdropOption};

use crate::app::{tokens, DbslocAppState, RunProgress, StageRunStatus};

pub use dbsloc_ui::{
    ants_programs_needed, finer_disabled_hover, finer_selection_warning, finer_than_voxel,
    grid_size_text, norm_custom_range, precoreg_needed, registration_method_label,
    stage_status_detail, stages_with_precoreg, stepped_grid_text, ImageKind, ImageRow, LeadReview,
    MetalContrast, NormResolution, PipelineReadiness, RecoSideSummary, ReconMethod,
    RegistrationEngine, RegistrationReview, ReviewMode, RunRequest, StageMeta, StageSettings,
    StageState, SubjectResult, UpstreamReviewWarning, EL_MODEL_FROM_SUBJECT_HOVER,
    EL_MODEL_FROM_SUBJECT_LABEL, REVIEW_MODE_HOVER, REVIEW_MODE_LABEL, REVIEW_REGISTRATIONS_HOVER,
    REVIEW_REGISTRATIONS_LABEL, STAGE_META,
};

/// One body-text line height — the layout unit for this GUI.
///
/// Every size below is a multiple of this, so the layout tracks the user's font
/// size and display scaling instead of fixed pixels.
pub fn em(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Body)
}

/// Status colours and surfaces for the current theme.
pub fn palette(ui: &egui::Ui) -> tokens::Palette {
    tokens::palette(ui.visuals().dark_mode)
}

/// A status chip: the glyph **and** the wording carry the meaning, so the state
/// stays readable without colour vision.
pub(crate) fn status_chip(ui: &mut egui::Ui, glyph: &str, text: &str, color: egui::Color32) {
    ui.add(
        egui::Label::new(egui::RichText::new(format!("{glyph}  {text}")).color(color))
            .wrap()
            .selectable(false),
    );
}

/// Fade the bottom of a scrolled region into `fill`, so content clipped by
/// whatever sits below it reads as continuing rather than as cut off.
fn fade_out_bottom(ui: &egui::Ui, region: egui::Rect, fill: egui::Color32, unit: f32) {
    let height = (unit * 1.2).min(region.height());
    let rect = egui::Rect::from_min_max(
        egui::pos2(region.left(), region.bottom() - height),
        region.max,
    );
    let transparent = egui::Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), transparent);
    mesh.colored_vertex(rect.right_top(), transparent);
    mesh.colored_vertex(rect.left_bottom(), fill);
    mesh.colored_vertex(rect.right_bottom(), fill);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(2, 1, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// A framed callout with a heading — used for prerequisites and warnings.
pub(crate) fn callout<R>(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    heading: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let pal = palette(ui);
    egui::Frame::group(ui.style())
        .fill(pal.surface_raised)
        .stroke(egui::Stroke::new(1.0_f32, pal.hairline))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(heading).strong().color(accent));
            ui.add_space(em(ui) * 0.2);
            body(ui)
        })
        .inner
}

pub(crate) use dbsloc_viz::widgets::info_icon;

/// Reveal `path` in the platform file manager. Best-effort: a failure is
/// reported to the caller so it can surface a real error rather than fail quietly.
pub fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    dbsloc_viz::widgets::reveal_in_file_manager(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirectoryEvent {
    None,
    Committed,
    Browse,
    Pick(String),
    PickDefault,
}

pub(crate) struct DirectoryRow<'a> {
    pub label: &'static str,
    pub field_id: egui::Id,
    pub hint: String,
    pub hover: &'static str,
    pub browse_hover: &'static str,
    pub recent_hover: &'static str,
    pub recent_id: &'static str,
    pub recents: &'a [String],
    pub empty_recents: &'static str,
    pub default_entry: Option<&'static str>,
    pub disabled: Option<&'a str>,
}

pub(crate) const OUTPUT_DIRECTORY_HOVER: &str =
    "Where every step writes its results; empty = derivatives/leaddbs inside the input directory";

pub(crate) fn directory_row(
    ui: &mut egui::Ui,
    row: &DirectoryRow<'_>,
    text: &mut String,
) -> DirectoryEvent {
    let unit = em(ui);
    let mut event = DirectoryEvent::None;
    ui.horizontal(|ui| {
        if row.disabled.is_some() {
            ui.disable();
        }
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            egui::vec2(unit * 8.5, height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(egui::Label::new(row.label).selectable(false));
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Browse…")
                .on_hover_text(row.browse_hover)
                .on_disabled_hover_text(row.disabled.unwrap_or_default())
                .clicked()
            {
                event = DirectoryEvent::Browse;
            }
            egui::ComboBox::from_id_salt(row.recent_id)
                .width(unit * 6.0)
                .selected_text("Recent")
                .show_ui(ui, |ui| {
                    if let Some(default) = row.default_entry {
                        if ui.selectable_label(false, default).clicked() {
                            event = DirectoryEvent::PickDefault;
                            ui.close();
                        }
                    }
                    if row.recents.is_empty() {
                        ui.add_enabled(false, egui::Label::new(row.empty_recents));
                    }
                    for dir in row.recents {
                        if ui.selectable_label(false, dir).clicked() {
                            event = DirectoryEvent::Pick(dir.clone());
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text(row.recent_hover)
                .on_disabled_hover_text(row.disabled.unwrap_or_default());
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let resp = ui
                    .add_sized(
                        [ui.available_width(), height],
                        egui::TextEdit::singleline(text)
                            .id(row.field_id)
                            .hint_text(row.hint.as_str()),
                    )
                    .on_hover_text(row.hover)
                    .on_disabled_hover_text(row.disabled.unwrap_or_default());
                if resp.lost_focus() {
                    event = DirectoryEvent::Committed;
                }
            });
        });
    });
    event
}

#[derive(Default)]
pub struct SubjectPanel {
    subjects: Vec<String>,
    filter: String,
    /// Filtered view of `subjects`, rebuilt only when the filter or the scan
    /// result changes — not cloned and lowercased on every frame.
    matches: Vec<String>,
    matches_key: Option<(String, usize)>,
    /// BIDS root and output directory the cached `subjects` belong to.
    scanned_key: Option<ScanKey>,
    listed_key: Option<ScanKey>,
    pending: Option<mpsc::Receiver<(ScanKey, Vec<String>)>>,
    scanning: bool,
}

type ScanKey = (String, Option<PathBuf>);

impl SubjectPanel {
    pub(crate) fn subjects(&self) -> &[String] {
        &self.subjects
    }

    pub(crate) fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Whether the finished scan of `bids_dir` lists `subject`.
    pub(crate) fn lists(&self, bids_dir: &str, subject: &str) -> bool {
        !self.scanning
            && self
                .scanned_key
                .as_ref()
                .is_some_and(|k| k.0 == bids_dir.trim())
            && self.subjects.iter().any(|s| s == subject)
    }

    /// Force a rescan on the next poll (bound to Refresh / F5).
    pub fn invalidate(&mut self) {
        self.scanned_key = None;
    }

    /// Kick off / collect the background BIDS scan. Never touches the filesystem
    /// on the calling (paint) thread.
    pub fn poll(&mut self, ctx: &egui::Context, state: &DbslocAppState) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok((key, subjects)) => {
                    self.pending = None;
                    self.scanning = false;
                    if Some(&key) == self.scanned_key.as_ref() {
                        self.subjects = subjects;
                        self.matches_key = None;
                        self.listed_key = Some(key);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.scanning = false;
                }
            }
        }

        let key: ScanKey = (state.bids_dir.trim().to_owned(), state.output_dir());
        if self.scanned_key.as_ref() == Some(&key) {
            return;
        }
        if self.listed_key.as_ref() != Some(&key) {
            self.subjects.clear();
            self.matches_key = None;
        }
        self.scanned_key = Some(key.clone());
        if key.0.is_empty() {
            return;
        }
        self.scanning = true;
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let root = PathBuf::from(&key.0);
            let ids = dbsloc_pipeline::bids::list_subject_ids_in(&root, key.1.as_deref());
            let subs: Vec<String> = ids.into_iter().map(|id| format!("sub-{id}")).collect();
            let _ = tx.send((key, subs));
            ctx.request_repaint();
        });
    }

    /// Move the selection by `delta` through the currently filtered list.
    pub fn step_selection(&mut self, state: &mut DbslocAppState, delta: isize) {
        self.refresh_matches();
        let visible: Vec<&String> = self.matches.iter().collect();
        if visible.is_empty() {
            return;
        }
        let cur = state
            .selected_subject
            .as_ref()
            .and_then(|s| visible.iter().position(|v| *v == s))
            .map(|i| i as isize);
        let next = match cur {
            Some(i) => (i + delta).clamp(0, visible.len() as isize - 1),
            None if delta >= 0 => 0,
            None => visible.len() as isize - 1,
        };
        state.selected_subject = Some(visible[next as usize].clone());
    }

    /// Rebuild [`Self::matches`] when — and only when — the inputs changed.
    fn refresh_matches(&mut self) {
        let key = (self.filter.trim().to_ascii_lowercase(), self.subjects.len());
        if self.matches_key.as_ref() == Some(&key) {
            return;
        }
        let needle = key.0.clone();
        self.matches = self
            .subjects
            .iter()
            .filter(|s| needle.is_empty() || s.to_ascii_lowercase().contains(&needle))
            .cloned()
            .collect();
        self.matches_key = Some(key);
    }

    /// Layout order matters: the fixed-height header is drawn first and the
    /// subject list then takes whatever height is left, so nothing has to
    /// reserve space and nothing can be clipped when the panel is short.
    ///
    /// Which steps already have output is deliberately *not* repeated here —
    /// each step card in the Pipeline panel states it for that step.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut DbslocAppState, locked: Option<&str>) {
        let unit = em(ui);
        let pal = palette(ui);

        if state.bids_dir.trim().is_empty() {
            ui.add_space(unit);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No input directory chosen yet.").color(pal.text_muted),
                );
                ui.label(
                    egui::RichText::new("Use Browse… in the toolbar, or press Ctrl+O.")
                        .small()
                        .color(pal.text_muted),
                );
            });
            return;
        }

        if self.scanning && self.subjects.is_empty() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(unit));
                ui.label("Scanning the dataset for subjects…");
            });
            return;
        }

        if self.subjects.is_empty() {
            ui.add_space(unit);
            callout(ui, pal.warn, "No subjects found", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "No sub-* folders under rawdata/ or the output directory.",
                        )
                        .small()
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
                if ui
                    .button("Scan again")
                    .on_hover_text("Look for subjects again (F5)")
                    .clicked()
                {
                    self.invalidate();
                }
            });
            return;
        }

        let total = self.subjects.len();
        ui.horizontal(|ui| {
            ui.label("🔍").on_hover_text("Filter the subject list");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.filter.is_empty(), egui::Button::new("✖").frame(false))
                    .on_hover_text("Clear the subject filter")
                    .clicked()
                {
                    self.filter.clear();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add_sized(
                        [ui.available_width(), ui.spacing().interact_size.y],
                        egui::TextEdit::singleline(&mut self.filter).hint_text("Filter subjects"),
                    )
                    .on_hover_text("Show only subjects whose identifier contains this text");
                });
            });
        });

        self.refresh_matches();
        let matches = std::mem::take(&mut self.matches);
        ui.separator();
        ui.label(
            egui::RichText::new(if matches.len() == total {
                format!("{total} subjects")
            } else {
                format!("{} of {total} subjects", matches.len())
            })
            .small()
            .color(pal.text_muted),
        );

        if let Some(reason) = locked {
            ui.label(egui::RichText::new(reason).small().color(pal.warn));
        }
        let bids = state.bids_dir.trim().to_owned();
        ui.add_enabled_ui(locked.is_none(), |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Select all")
                    .on_hover_text("Tick every subject shown, to run them one after another")
                    .clicked()
                {
                    state.checked_subjects.set_many(&bids, &matches, true);
                }
                if ui
                    .button("Select none")
                    .on_hover_text("Untick every subject")
                    .clicked()
                {
                    let all = self.subjects.clone();
                    state.checked_subjects.set_many(&bids, &all, false);
                }
                let n = state.checked_subjects.ordered(&bids, &self.subjects).len();
                ui.label(
                    egui::RichText::new(match n {
                        0 => "none ticked".to_owned(),
                        1 => "1 ticked".to_owned(),
                        n => format!("{n} ticked"),
                    })
                    .small()
                    .color(pal.text_muted),
                )
                .on_hover_text(
                    "Tick two or more subjects to run the selected steps for each of them, \
                     one subject after another. The highlighted subject is the one the step \
                     cards and the 3D viewer show.",
                );
            });
        });
        let picked = ui
            .add_enabled_ui(locked.is_none(), |ui| {
                checked_subject_rows(
                    ui,
                    &matches,
                    state.selected_subject.as_deref(),
                    &mut state.checked_subjects,
                    &bids,
                )
            })
            .inner;
        if let Some(sub) = picked.filter(|_| locked.is_none()) {
            state.selected_subject = Some(sub);
        }
        self.matches = matches;
    }
}

/// The Subjects tab's rows: a tick box that adds the subject to a run of several subjects,
/// and the name, which views the subject. Returns the subject the user clicked this frame.
fn checked_subject_rows(
    ui: &mut egui::Ui,
    subjects: &[String],
    selected: Option<&str>,
    checks: &mut dbsloc_ui::SubjectChecks,
    bids_dir: &str,
) -> Option<String> {
    let unit = em(ui);
    let mut picked = None;
    let row_h = (unit + 2.0 * ui.spacing().button_padding.y).max(ui.spacing().interact_size.y);
    egui::ScrollArea::vertical()
        .id_salt("subject_list")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, subjects.len(), |ui, range| {
            for sub in &subjects[range] {
                ui.horizontal(|ui| {
                    let mut on = checks.is_checked(bids_dir, sub);
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text(format!("Include {sub} when running several subjects"))
                        .changed()
                    {
                        checks.set(bids_dir, sub, on);
                        if on && selected.is_none() {
                            picked = Some(sub.clone());
                        }
                    }
                    let is_selected = selected == Some(sub.as_str());
                    let resp = ui
                        .selectable_label(is_selected, sub)
                        .on_hover_text("View this subject in the pipeline (Alt+Up / Alt+Down)");
                    if resp.clicked() && !is_selected {
                        picked = Some(sub.clone());
                    }
                });
            }
        });
    picked
}

/// The scrolling list of subject rows, shared by the Subjects tab and the
/// landing screen. Returns the subject the user clicked this frame, if any.
pub(crate) fn subject_rows(
    ui: &mut egui::Ui,
    id_salt: &str,
    subjects: &[String],
    selected: Option<&str>,
    hover: &str,
) -> Option<String> {
    let unit = em(ui);
    let mut picked = None;
    let row_h = unit + 2.0 * ui.spacing().button_padding.y;
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .show_rows(ui, row_h, subjects.len(), |ui, range| {
            for sub in &subjects[range] {
                let is_selected = selected == Some(sub.as_str());
                let resp = ui.selectable_label(is_selected, sub).on_hover_text(hover);
                if resp.clicked() && !is_selected {
                    picked = Some(sub.clone());
                }
            }
        });
    picked
}

/// How much of the stored output the Output tab shows. This is a **view** filter
/// only: every line the pipeline emits is kept, so changing it never loses text.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputDetail {
    #[default]
    Standard,
    Detailed,
    Debug,
}

impl OutputDetail {
    fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Detailed => "Detailed",
            Self::Debug => "Debug",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Standard => "Step results, warnings and errors only.",
            Self::Detailed => "Everything the external programs print.",
            Self::Debug => "Everything, with the stdout / stderr stream markers kept.",
        }
    }
}

/// Action returned by [`StagePanel::show`].
#[derive(Debug)]
pub enum StagePanelAction {
    None,
    Run(RunRequest),
    /// Ask for confirmation first: the selected steps would replace existing output.
    ConfirmOverwriteThenRun(RunRequest, Vec<Stage>),
    /// Request a cooperative cancel **between** steps.
    Cancel,
    BrowseTemplate,
    BrowseTemplateT2,
    RevealOutputDirectory,
    OpenStageFolder(Stage),
    ReviewNow,
    ReviewStage(Stage),
    RerunStep(Stage),
    RefineLocalization,
    OpenStimulation,
    /// Run the request for every ticked subject, one after another.
    RunBatch(RunRequest),
    SkipBatchReview,
    EndBatch,
    DismissBatchResults,
}

pub(crate) const STOP_HOVER: &str = "Stop the run (Ctrl+.). A running external program is \
     ended at once, a step on the graphics card stops at its next checkpoint, and the remaining \
     steps are skipped.";

/// Shown in the electrode picker when nothing is selected.
const NO_ELECTRODE_LABEL: &str = "(none — required)";

/// What the Pipeline panel shows about a run of several subjects; set by the app each frame.
#[derive(Default)]
pub struct BatchInfo {
    /// The ticked subjects in list order; two or more make Run a run of several subjects.
    pub subjects: Vec<String>,
    /// `Subject 2/5 · sub-03` while a subject of the batch runs.
    pub progress: Option<String>,
    /// The review the batch waits for, and how many follow it.
    pub review: Option<(String, Stage, usize)>,
    pub results: Vec<SubjectResult>,
    pub summary: Option<String>,
    /// A batch is running or reviewing.
    pub active: bool,
}

impl BatchInfo {
    pub fn is_batch(&self) -> bool {
        self.subjects.len() >= 2
    }
}

pub(crate) fn batch_result_row(ui: &mut egui::Ui, r: &SubjectResult, pal: &tokens::Palette) {
    let color = match r.phase {
        _ if r.rework => pal.warn,
        dbsloc_ui::BatchPhase::Done => pal.ok,
        dbsloc_ui::BatchPhase::AwaitingQc => pal.info,
        dbsloc_ui::BatchPhase::Cancelled => pal.warn,
        dbsloc_ui::BatchPhase::Failed | dbsloc_ui::BatchPhase::PlanError => pal.err,
    };
    ui.label(egui::RichText::new(&r.subject).small());
    ui.label(egui::RichText::new(r.status_text()).small().color(color))
        .on_hover_text(r.message.as_deref().unwrap_or("No message"));
    ui.label(egui::RichText::new(format!("{}/{}/{}", r.ok, r.failed, r.skipped)).small())
        .on_hover_text("Steps completed / failed / skipped");
    ui.label(egui::RichText::new(format!("{:.0} s", r.seconds)).small());
    ui.end_row();
}

/// Walk the derivatives tree and fill a [`PipelineReadiness`]. **Worker-thread
/// only** — this does real I/O. The pure data type lives in `dbsloc-ui`; only
/// the filesystem scan stays here.
pub(crate) fn scan_readiness(
    bids_dir: &str,
    output_dir: Option<&Path>,
    subject: &str,
    template: Option<&str>,
) -> PipelineReadiness {
    let bids_root = Path::new(bids_dir);
    let resolved = if subject.is_empty() {
        None
    } else {
        dbsloc_pipeline::bids::resolve_subject_in(bids_root, output_dir, subject, true).ok()
    };
    let raw_subj = if bids_root.join("rawdata").is_dir() {
        bids_root.join("rawdata").join(subject)
    } else {
        bids_root.join(subject)
    };
    let have_subject = !subject.is_empty();
    let (raw_ct, raw_truncated) = if have_subject {
        scan_for_file(&raw_subj, is_ct_nifti_name)
    } else {
        (false, false)
    };

    let reco = resolved
        .as_ref()
        .and_then(|s| read_json(&s.reconstruction_json()));
    let reviews = resolved.as_ref().map_or(
        (
            RegistrationReview::NotReviewed,
            RegistrationReview::NotReviewed,
        ),
        registration_reviews,
    );

    let reco_sides = reco.as_ref().map(reco_sides_in).unwrap_or_default();
    PipelineReadiness {
        scan_truncated: raw_truncated,
        has_bids_dir: !bids_dir.trim().is_empty() && bids_root.is_dir(),
        has_subject: have_subject,
        template_exists: template.is_some_and(|p| Path::new(p).is_file()),
        has_preprocess_nifti: resolved
            .as_ref()
            .is_some_and(|s| s.preop_anchor_preproc.is_some()),
        has_coreg_warped: resolved.as_ref().is_some_and(|s| {
            !s.coreg.anat_postop.is_empty() || s.coreg.anat_preop.contains_key("T2w")
        }),
        has_raw_anat: have_subject
            && any_file_matching(&raw_subj, |name| {
                is_nifti_name(&name.to_ascii_lowercase()) && !is_ct_nifti_name(name)
            }),
        has_raw_postop_ct: raw_ct,
        has_raw_postop_mr: resolved
            .as_ref()
            .is_some_and(|s| !s.postop_mr_images().is_empty()),
        has_norm_nifti: resolved
            .as_ref()
            .is_some_and(|s| !s.norm.anat_preop.is_empty() || !s.norm.anat_postop.is_empty()),
        has_norm_postop_ct: resolved
            .as_ref()
            .is_some_and(|s| s.norm.anat_postop.contains_key("CT")),
        has_norm_postop_mr: resolved
            .as_ref()
            .is_some_and(|s| s.norm_postop_mr_in_use().is_some()),
        has_reconstruction_json: resolved
            .as_ref()
            .is_some_and(|s| s.reconstruction_json().is_file()),
        reco_manually_corrected: reco.as_ref().is_some_and(manually_corrected_in),
        has_orientation_json: reco.as_ref().is_some_and(orientation_in),
        has_vat_preview: false,
        stimulation_labels: resolved
            .as_ref()
            .map(|s| {
                let id = s.subj_id.strip_prefix("sub-").unwrap_or(&s.subj_id);
                dbsloc_pipeline::vat::settings::list_labels(&s.subj_dir, id)
                    .into_iter()
                    .map(|entry| entry.label)
                    .collect()
            })
            .unwrap_or_default(),
        stimulation_computed: resolved
            .as_ref()
            .map(|s| {
                let id = s.subj_id.strip_prefix("sub-").unwrap_or(&s.subj_id);
                let Some(space) = s.norm.template_space.as_deref() else {
                    return Vec::new();
                };
                dbsloc_pipeline::vat::settings::list_labels(&s.subj_dir, id)
                    .into_iter()
                    .map(|entry| entry.label)
                    .filter(|label| {
                        !crate::stimulation_panel::computed_files(
                            &dbsloc_pipeline::vat::settings::label_dir(&s.subj_dir, space, label),
                        )
                        .is_empty()
                    })
                    .collect()
            })
            .unwrap_or_default(),
        has_scene_manifest: resolved
            .as_ref()
            .is_some_and(|s| dbsloc_pipeline::derivatives::scene_manifest(s).is_file()),
        coreg_review: reviews.0,
        norm_review: reviews.1,
        images: resolved.as_ref().map(subject_images).unwrap_or_default(),
        auto_postop_is_ct: resolved
            .as_ref()
            .is_some_and(|s| s.postop_modality.as_deref() == Some("CT")),
        has_ants_registration: dbsloc_pipeline::bundled_bin::find_binary("antsRegistration")
            .is_some(),
        has_n4_bias_correction: dbsloc_pipeline::bundled_bin::find_binary("N4BiasFieldCorrection")
            .is_some(),
        gpu_available: dbsloc_pipeline::reg_backend::cached_probe_if_ready().map(|p| p.available),
        reco_sides,
    }
}

fn lead_label(key: &str) -> &'static str {
    if key == "1" {
        "Right lead"
    } else {
        "Left lead"
    }
}

fn reco_sides_in(value: &serde_json::Value) -> Vec<RecoSideSummary> {
    let block = if value
        .get("native")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|b| b.values().any(serde_json::Value::is_object))
    {
        "native"
    } else {
        "mni"
    };
    let mut out = Vec::new();
    for key in ["1", "2"] {
        let flags: Vec<String> = value["meta"]["reconstruct"]["flags"][key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if value[block]
            .get(key)
            .is_some_and(serde_json::Value::is_object)
        {
            let review = match dbsloc_pipeline::manual_correction::side_review(value, key) {
                Some(dbsloc_pipeline::manual_correction::REVIEW_ACCEPTED) => LeadReview::Accepted,
                Some(_) => LeadReview::Corrected,
                None => LeadReview::Unchecked,
            };
            out.push(RecoSideSummary {
                label: lead_label(key),
                review,
                reason: None,
                flags,
            });
        } else if let Some(why) = value["meta"]["reconstruct"]["failed"][key].as_str() {
            out.push(RecoSideSummary {
                label: lead_label(key),
                review: LeadReview::NotFound,
                reason: Some(why.to_owned()),
                flags,
            });
        }
    }
    out
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn orientation_in(value: &serde_json::Value) -> bool {
    value
        .get("meta")
        .and_then(|m| m.get("orientation"))
        .is_some_and(serde_json::Value::is_object)
}

fn manually_corrected_in(value: &serde_json::Value) -> bool {
    value
        .get("props")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|props| {
            props.values().any(|side| {
                side.get("manually_corrected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
        })
}

fn review_of(sub: &dbsloc_pipeline::bids::Subject, stage: Stage) -> RegistrationReview {
    use dbsloc_pipeline::ReviewState;
    match dbsloc_pipeline::method_log_path(sub, stage).map(|p| dbsloc_pipeline::review_state(&p)) {
        Some(ReviewState::Approved) => RegistrationReview::Approved,
        Some(ReviewState::Rejected) => RegistrationReview::Rejected,
        Some(ReviewState::NotReviewed) | None => RegistrationReview::NotReviewed,
    }
}

pub(crate) fn registration_reviews(
    sub: &dbsloc_pipeline::bids::Subject,
) -> (RegistrationReview, RegistrationReview) {
    (
        review_of(sub, Stage::Coregister),
        review_of(sub, Stage::Normalize),
    )
}

fn is_nifti_name(name: &str) -> bool {
    name.ends_with(".nii") || name.ends_with(".nii.gz")
}

/// Recognise a CT image by its **BIDS suffix** (`…_ct.nii.gz`).
///
/// Entities never decide the modality: `sub-01_acq-postop_T1w.nii.gz` is a
/// post-operative *MRI*, and treating it as a CT made the pipeline promise a
/// coregistration it could not run.
fn is_ct_nifti_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if !is_nifti_name(&n) {
        return false;
    }
    let stem = n
        .strip_suffix(".gz")
        .unwrap_or(&n)
        .strip_suffix(".nii")
        .unwrap_or("");
    let suffix = stem.rsplit('_').next().unwrap_or("");
    suffix == "ct"
}

/// Bounded recursive search: stops at the first hit, and refuses to descend
/// further than a BIDS subject tree ever needs, so a stray symlink or a huge
/// sibling dataset cannot turn this into an unbounded walk.
fn any_file_matching(root: &Path, pred: impl Fn(&str) -> bool) -> bool {
    scan_for_file(root, pred).0
}

/// `(found, truncated)` — `truncated` means the entry budget ran out, so a
/// `false` result is "not seen", not "not there".
fn scan_for_file(root: &Path, pred: impl Fn(&str) -> bool) -> (bool, bool) {
    const MAX_DEPTH: u32 = 6;
    const MAX_ENTRIES: u32 = 50_000;
    let mut seen = 0_u32;
    let mut stack = vec![(root.to_path_buf(), 0_u32)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return (false, true);
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            } else if ft.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                if pred(&name) {
                    return (true, false);
                }
            }
        }
    }
    (false, false)
}

type SubjectKey = (String, String, Option<PathBuf>);
type ReadinessKey = (SubjectKey, String);
type ReadinessMsg = (ReadinessKey, PipelineReadiness);

fn subject_key(state: &DbslocAppState) -> SubjectKey {
    (
        state.bids_dir.trim().to_owned(),
        state.selected_subject.clone().unwrap_or_default(),
        state.output_dir(),
    )
}

fn subject_images(s: &dbsloc_pipeline::bids::Subject) -> Vec<ImageRow> {
    let mut rows: Vec<ImageRow> = Vec::new();
    let mut add = |kind: ImageKind, label: String, path: &Path, is_ct: bool| {
        if path.is_file() && !rows.iter().any(|r| r.path == path) {
            rows.push(ImageRow {
                kind,
                label,
                path: path.to_path_buf(),
                is_ct,
            });
        }
    };
    let anchor = s.anchor_modality.clone();
    if let Some(p) = s.anchor_in_use() {
        add(
            ImageKind::PreopAnchor,
            format!("Pre-op {anchor} (anchor in use)"),
            &p,
            false,
        );
    }
    if let Some(p) = &s.preop_anchor_raw {
        add(
            ImageKind::PreopAnchor,
            format!("Pre-op {anchor} (original)"),
            p,
            false,
        );
    }
    if let Some(p) = &s.preop_anchor_preproc {
        add(
            ImageKind::Derived,
            format!("Pre-op {anchor} (bias-corrected)"),
            p,
            false,
        );
    }
    for (key, p) in s.preop_raw.iter() {
        if *key != anchor {
            add(ImageKind::PreopOther, format!("Pre-op {key}"), p, false);
        }
    }
    if let Some(p) = &s.postop_ct_raw {
        add(ImageKind::PostopCt, "Post-op CT".to_owned(), p, true);
    }
    for (key, p) in s.postop_mr_images() {
        let orientation = match key {
            "ax_MRI" => "axial",
            "cor_MRI" => "coronal",
            _ => "sagittal",
        };
        add(
            ImageKind::PostopMr,
            format!("Post-op MRI ({orientation})"),
            p,
            false,
        );
    }
    for (key, p) in s.coreg.anat_preop.iter() {
        add(
            ImageKind::Derived,
            format!("Pre-op {key} (coregistered)"),
            p,
            false,
        );
    }
    for (key, p) in s.coreg.anat_postop.iter() {
        add(
            ImageKind::Derived,
            format!("Post-op {key} (coregistered)"),
            p,
            key == "CT",
        );
    }
    rows
}

/// What a *View* button asks the application to show.
#[derive(Clone, Debug, Default)]
pub(crate) struct ImagePreviewRequest {
    pub path: PathBuf,
    pub label: String,
    pub is_ct: bool,
}

fn preview_request_id() -> egui::Id {
    egui::Id::new("image_preview_request")
}

pub(crate) fn request_image_preview(ctx: &egui::Context, row: &ImageRow) {
    let request = ImagePreviewRequest {
        path: row.path.clone(),
        label: row.label.clone(),
        is_ct: row.is_ct,
    };
    ctx.data_mut(|d| d.insert_temp(preview_request_id(), request));
    ctx.request_repaint();
}

pub(crate) fn image_view_button(ui: &mut egui::Ui, row: &ImageRow) {
    if ui
        .button("View")
        .on_hover_text(format!("Show the slices of {}", row.path.display()))
        .clicked()
    {
        request_image_preview(ui.ctx(), row);
    }
}

fn image_file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn subject_images_section(ui: &mut egui::Ui, readiness: &PipelineReadiness, pal: &tokens::Palette) {
    if readiness.images.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(format!("Subject images ({})", readiness.images.len()))
        .id_salt("pipeline_subject_images")
        .default_open(false)
        .show(ui, |ui| {
            for row in &readiness.images {
                image_row_line(ui, row, pal);
            }
        })
        .header_response
        .on_hover_text(
            "The image files this subject's steps read and write; View shows the slices",
        );
    ui.add_space(em(ui) * 0.4);
}

/// One image row: label, truncated file name, *View*.
pub(crate) fn image_row_line(ui: &mut egui::Ui, row: &ImageRow, pal: &tokens::Palette) {
    ui.horizontal(|ui| {
        let unit = em(ui);
        ui.label(egui::RichText::new(format!("{}:", row.label)).color(pal.text_muted));
        let room = (ui.available_width() - unit * 4.5).max(unit * 4.0);
        ui.add_sized(
            [room, unit * 1.3],
            egui::Label::new(image_file_name(&row.path)).truncate(),
        )
        .on_hover_text(row.path.display().to_string());
        image_view_button(ui, row);
    });
}

/// Owns the one floating image preview of the application.
#[derive(Default)]
pub struct ImagePreviewHost {
    preview: Option<VolumePreview>,
}

impl ImagePreviewHost {
    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(r) =
            ctx.data_mut(|d| d.remove_temp::<ImagePreviewRequest>(preview_request_id()))
        {
            self.open(ctx, r);
        }
        let Some(preview) = &mut self.preview else {
            return;
        };
        let mut open = true;
        let screen = ctx.content_rect().size();
        egui::Window::new(preview.title().to_owned())
            .id(egui::Id::new("image_preview_window"))
            .open(&mut open)
            .resizable(true)
            .default_size(screen * 0.6)
            .show(ctx, |ui| preview.show(ui));
        if !open {
            self.preview = None;
        }
    }

    fn open(&mut self, ctx: &egui::Context, r: ImagePreviewRequest) {
        let title = format!("Image preview — {}", r.label);
        let ct_hint = r.is_ct.then_some(true);
        self.preview = Some(VolumePreview::open(r.path, title, ct_hint, ctx));
    }

    #[cfg(test)]
    pub fn current_file(&self) -> Option<&str> {
        self.preview.as_ref().map(VolumePreview::file_name)
    }
}

/// Caches [`PipelineReadiness`] and refreshes it on a worker thread whenever the
/// dataset, subject or template changes, a step finishes, or the user asks.
#[derive(Default)]
pub struct ReadinessCache {
    current: PipelineReadiness,
    current_for: Option<SubjectKey>,
    key: Option<ReadinessKey>,
    pending: Option<mpsc::Receiver<ReadinessMsg>>,
    scanning: bool,
    generation: u64,
    answered: u64,
    retried: Option<u64>,
}

impl ReadinessCache {
    pub fn get(&self) -> &PipelineReadiness {
        &self.current
    }

    /// Drop the cached value so the next [`Self::poll`] rescans.
    pub fn invalidate(&mut self) {
        self.key = None;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn is_current_for(&self, state: &DbslocAppState) -> bool {
        self.answered == self.generation && self.current_for.as_ref() == Some(&subject_key(state))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn forget_other_subject(&mut self, state: &DbslocAppState) {
        let owner = subject_key(state);
        if self.current_for.as_ref() != Some(&owner) {
            self.current = PipelineReadiness::default();
            self.current_for = None;
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, state: &DbslocAppState) {
        self.forget_other_subject(state);
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok((key, readiness)) => {
                    self.pending = None;
                    self.scanning = false;
                    if Some(&key) == self.key.as_ref() {
                        self.current = readiness;
                        self.current_for = Some(key.0);
                        self.answered = self.generation;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.scanning = false;
                    if self.retried != Some(self.generation) {
                        self.retried = Some(self.generation);
                        self.key = None;
                    }
                }
            }
        }

        let key: ReadinessKey = (
            subject_key(state),
            state.normalization_template.clone().unwrap_or_default(),
        );
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.key = Some(key.clone());
        self.scanning = true;
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let template = if key.1.is_empty() {
                None
            } else {
                Some(key.1.as_str())
            };
            let (bids, subject, output) = &key.0;
            let readiness = scan_readiness(bids, output.as_deref(), subject, template);
            let _ = tx.send((key, readiness));
            ctx.request_repaint();
        });
    }
}

#[derive(Default)]
pub struct StagePanel {
    pub running: bool,
    /// A stop was requested; the current step still has to finish.
    pub stopping: bool,
    /// Id of the Run button, so the screenshot hook can put keyboard focus on a
    /// real control rather than guessing at one.
    pub run_button_id: Option<egui::Id>,
    pub review_preparing: Option<Stage>,
    pub refine_activity: RefineActivity,
    pub run_wait: Option<&'static str>,
    /// Graphics status and the driver notice, shared with the start screen.
    pub gpu_help: crate::gpu_status::GpuHelp,
    pub batch: BatchInfo,
}

impl StagePanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut DbslocAppState,
        readiness: &PipelineReadiness,
        progress: &RunProgress,
    ) -> StagePanelAction {
        let mut action = StagePanelAction::None;
        let unit = em(ui);
        let pal = palette(ui);

        if state.selected_subject.is_none() {
            ui.add_space(unit);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("Select a subject to configure a run.")
                        .color(pal.text_muted),
                );
            });
            return action;
        }

        let (runnable_stages, blockers, unreachable) = if self.batch.active {
            (state.stage.selected_stages(), Vec::new(), Vec::new())
        } else if self.batch.is_batch() {
            (
                state.stage.selected_stages(),
                state.stage.batch_blockers(readiness.has_bids_dir),
                Vec::new(),
            )
        } else {
            let runnable = state.stage.selected_runnable_stages(readiness);
            let blockers = state.stage.selection_blockers(readiness, &runnable);
            (
                runnable,
                blockers,
                state.stage.unreachable_stages(readiness),
            )
        };

        egui::Panel::bottom("pipeline_actions")
            .frame(
                egui::Frame::NONE
                    .fill(pal.surface)
                    .inner_margin(egui::Margin::symmetric(unit as i8, (unit * 0.4) as i8)),
            )
            .max_size(ui.available_height() * 0.7)
            .show_separator_line(true)
            .show(ui, |ui| {
                let top = ui.max_rect();
                ui.painter().hline(
                    top.x_range(),
                    top.top(),
                    egui::Stroke::new(1.0_f32, pal.hairline_strong),
                );
                self.run_controls(
                    ui,
                    state,
                    readiness,
                    progress,
                    &runnable_stages,
                    &blockers,
                    &unreachable,
                    &mut action,
                );
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let region = ui.max_rect();
                let scrolled = egui::ScrollArea::vertical()
                    .id_salt("pipeline_steps")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.environment_banner(ui, readiness, &state.stage, &pal);
                        let browse = self.steps(ui, state, readiness, progress, &mut action);
                        self.run_options(ui, state);
                        ui.add_space(unit);
                        match browse {
                            NormalizeBrowse::Template => action = StagePanelAction::BrowseTemplate,
                            NormalizeBrowse::TemplateT2 => {
                                action = StagePanelAction::BrowseTemplateT2;
                            }
                            NormalizeBrowse::Stimulation => {
                                action = StagePanelAction::OpenStimulation;
                            }
                            NormalizeBrowse::None => {}
                        }
                    });
                let more_below = scrolled.content_size.y - scrolled.inner_rect.height() > 1.0
                    && scrolled.state.offset.y + scrolled.inner_rect.height()
                        < scrolled.content_size.y - 1.0;
                if more_below {
                    fade_out_bottom(ui, region, pal.surface, unit);
                }
            });
        action
    }

    /// Draws the step cards. Returns which template file the user asked to browse
    /// for, if any.
    fn steps(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut DbslocAppState,
        readiness: &PipelineReadiness,
        progress: &RunProgress,
        action: &mut StagePanelAction,
    ) -> NormalizeBrowse {
        let unit = em(ui);
        let pal = palette(ui);
        let mut browse = NormalizeBrowse::None;
        subject_images_section(ui, readiness, &pal);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.running, egui::Button::new("Select all steps"))
                .on_hover_text("Tick every pipeline step")
                .clicked()
            {
                state.stage.set_all(true);
            }
            if ui
                .add_enabled(!self.running, egui::Button::new("Clear selection"))
                .on_hover_text("Untick every pipeline step")
                .clicked()
            {
                state.stage.set_all(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let any_open = STAGE_META.iter().any(|meta| {
                    egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        egui::Id::new(("step_card", meta.id)),
                        true,
                    )
                    .is_open()
                });
                if ui
                    .button(if any_open {
                        "Collapse all"
                    } else {
                        "Expand all"
                    })
                    .on_hover_text("Fold or unfold every step card")
                    .clicked()
                {
                    for meta in &STAGE_META {
                        let mut fold =
                            egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                egui::Id::new(("step_card", meta.id)),
                                true,
                            );
                        fold.set_open(!any_open);
                        fold.store(ui.ctx());
                    }
                }
            });
        });
        ui.add_space(unit * 0.4);

        for meta in &STAGE_META {
            let (status, status_text) = stage_status_detail(meta.stage, &state.stage, readiness);
            let run_status = progress.status_of(meta.stage);
            let mut selected = state.stage.selected(meta.stage);
            let enabled = !self.running;
            let will_skip = selected
                && !state.stage.overwrite_results
                && readiness.stage_output_exists(meta.stage);

            let review = card_review(
                readiness,
                &state.stage,
                meta.stage,
                self.running,
                self.review_preparing,
                self.refine_activity,
            );
            let card = stage_card(
                ui,
                meta,
                status,
                &status_text,
                run_status,
                progress.skip_reason(meta.stage),
                enabled,
                will_skip,
                &review,
                &pal,
                &mut selected,
                CardMode::Selectable,
                |ui, part| match part {
                    CardPart::Body => {
                        if meta.stage == Stage::Vat {
                            let wanted = vat_card_body(ui, state, readiness, &pal);
                            if wanted != NormalizeBrowse::None {
                                browse = wanted;
                            }
                        }
                    }
                    CardPart::Options => {
                        let wanted = stage_options(
                            meta.stage,
                            ui,
                            state,
                            &pal,
                            CardMode::Selectable,
                            readiness,
                        );
                        if wanted != NormalizeBrowse::None {
                            browse = wanted;
                        }
                    }
                },
            );
            if card != CardAction::None {
                *action = card_to_panel_action(card, meta.stage);
            }

            state.stage.set_selected(meta.stage, selected);
        }
        browse
    }

    fn run_options(&mut self, ui: &mut egui::Ui, state: &mut DbslocAppState) {
        ui.add_space(em(ui) * 0.4);
        ui.separator();
        ui.label(egui::RichText::new("Run options").strong());
        ui.add_enabled_ui(!self.running, |ui| {
            registration_shared_options(ui, state, &self.gpu_help);
            ui.add_space(em(ui) * 0.2);
            ui.checkbox(
                &mut state.stage.overwrite_results,
                "Rerun all selected steps",
            )
            .on_hover_text(
                "Run every ticked step again, even when its output already exists or was \
                 approved. Its output is replaced; you are asked to confirm first.",
            );
            review_mode_row(ui, &mut state.stage.review_mode);
            ui.checkbox(&mut state.stage.stop_on_error, "Stop on first error")
                .on_hover_text("Skip the remaining steps once a step fails.");
        });
    }

    fn batch_section(&self, ui: &mut egui::Ui, action: &mut StagePanelAction) {
        let unit = em(ui);
        let pal = palette(ui);
        if let Some(label) = &self.batch.progress {
            ui.add_space(unit * 0.3);
            ui.label(egui::RichText::new(label).strong().color(pal.info))
                .on_hover_text(
                    "The subject now running, and its place in this run of several subjects",
                );
        }
        if let Some((subject, stage, more)) = &self.batch.review {
            ui.add_space(unit * 0.3);
            callout(ui, pal.info, "Registration reviews after the run", |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{subject}: {} — approve or reject it in the Registration review tab{}",
                        stage.label(),
                        match more {
                            0 => String::new(),
                            1 => "; 1 more follows".to_owned(),
                            n => format!("; {n} more follow"),
                        }
                    ))
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .button("Skip this review")
                        .on_hover_text("Leave this registration unreviewed and go to the next one; the subject stays awaiting review")
                        .clicked()
                    {
                        *action = StagePanelAction::SkipBatchReview;
                    }
                    if ui
                        .button("End the batch")
                        .on_hover_text("Stop here: no further reviews open and no remaining steps run")
                        .clicked()
                    {
                        *action = StagePanelAction::EndBatch;
                    }
                });
            });
        }
        if self.batch.results.is_empty() {
            return;
        }
        ui.add_space(unit * 0.3);
        let title = self
            .batch
            .summary
            .clone()
            .unwrap_or_else(|| format!("{} subjects", self.batch.results.len()));
        egui::CollapsingHeader::new(egui::RichText::new(title).small())
            .id_salt("batch_results")
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("batch_results_scroll")
                    .max_height(unit * 8.0)
                    .show(ui, |ui| {
                        egui::Grid::new("batch_results_grid")
                            .num_columns(4)
                            .spacing([unit * 0.8, unit * 0.15])
                            .show(ui, |ui| {
                                for head in ["Subject", "Result", "Steps", "Time"] {
                                    ui.label(egui::RichText::new(head).small().strong());
                                }
                                ui.end_row();
                                for r in &self.batch.results {
                                    batch_result_row(ui, r, &pal);
                                }
                            });
                    });
                if !self.batch.active
                    && ui
                        .small_button("Dismiss")
                        .on_hover_text("Hide this list of subject results")
                        .clicked()
                {
                    *action = StagePanelAction::DismissBatchResults;
                }
            });
    }

    fn environment_banner(
        &self,
        ui: &mut egui::Ui,
        readiness: &PipelineReadiness,
        settings: &StageSettings,
        pal: &tokens::Palette,
    ) {
        if readiness.scan_truncated {
            callout(ui, pal.warn, "Subject folder only partly scanned", |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "This subject folder has more files than the scan budget, so a \
                             step reported as missing its input may in fact have one. Point \
                             the dataset directory at the BIDS root rather than a parent.",
                        )
                        .small()
                        .color(pal.text_muted),
                    )
                    .wrap(),
                );
            });
            ui.add_space(em(ui) * 0.4);
        }
        let needs_ants = |stage: Stage| {
            settings.selected(stage)
                && ants_programs_needed(settings.engine_for(stage), readiness.gpu_available)
        };
        let n4_missing = needs_ants(Stage::Preprocess) && !readiness.has_n4_bias_correction;
        let precoreg_needs_ants = precoreg_needed(&settings.selected_stages(), readiness)
            && ants_programs_needed(settings.preprocess_engine, readiness.gpu_available);
        let ants_missing = ([Stage::Coregister, Stage::Normalize]
            .into_iter()
            .any(needs_ants)
            || precoreg_needs_ants)
            && !readiness.has_ants_registration;
        if !n4_missing && !ants_missing {
            return;
        }
        callout(ui, pal.err, "External programs missing", |ui| {
            if n4_missing {
                status_chip(ui, "✖", "N4BiasFieldCorrection not found", pal.err);
            }
            if ants_missing {
                status_chip(ui, "✖", "antsRegistration not found", pal.err);
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Install ANTs, or set DBSLOC_ANTS_DIR to the directory that contains \
                         these programs. Steps that need them cannot run.",
                    )
                    .small()
                    .color(pal.text_muted),
                )
                .wrap(),
            );
        });
        ui.add_space(em(ui) * 0.4);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_controls(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut DbslocAppState,
        readiness: &PipelineReadiness,
        progress: &RunProgress,
        runnable_stages: &[Stage],
        blockers: &[String],
        unreachable: &[Stage],
        action: &mut StagePanelAction,
    ) {
        let unit = em(ui);
        let pal = palette(ui);

        if !blockers.is_empty() {
            ui.add_space(unit * 0.3);
            callout(ui, pal.warn, "Before this run can start", |ui| {
                for b in blockers {
                    status_chip(ui, "⚠", b, pal.warn);
                }
                if !unreachable.is_empty() {
                    ui.add_space(unit * 0.2);
                    if ui
                        .add_enabled(
                            !self.running,
                            egui::Button::new("Untick the steps that cannot run"),
                        )
                        .on_hover_text(
                            "Remove exactly the steps listed above from the selection. \
                             Nothing else is changed.",
                        )
                        .clicked()
                    {
                        state.stage.deselect_unreachable(readiness);
                    }
                }
            });
        }

        let batch = self.batch.is_batch() && !self.running;
        if !self.running && !self.batch.active && !runnable_stages.is_empty() && blockers.is_empty()
        {
            let names: Vec<&str> = runnable_stages.iter().map(|s| s.label()).collect();
            let text = if batch {
                format!(
                    "Will run for {} subjects, one after another: {}",
                    self.batch.subjects.len(),
                    names.join(", ")
                )
            } else {
                format!("Will run: {}", names.join(", "))
            };
            let label = ui.add(
                egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted)).wrap(),
            );
            if batch {
                label.on_hover_text(
                    "Each subject is judged on its own files when its turn comes: a step whose                      output already exists is skipped for that subject unless \"Rerun all                      selected steps\" is ticked. A subject that fails is reported and the next                      one starts.",
                );
            }
        }

        self.batch_section(ui, action);

        if self.running || progress.has_result() {
            ui.add_space(unit * 0.3);
            progress_bar(ui, progress, self.running);
        }

        ui.add_space(unit * 0.3);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().button_padding.x = em(ui) * 0.2;
            let run_enabled = !self.running
                && !self.batch.active
                && blockers.is_empty()
                && self.run_wait.is_none();
            let btn_h = unit * 1.9;
            let batch_label = format!("▶ Run ({} subjects)", self.batch.subjects.len());
            let run_label = if self.running {
                "▶ Running…"
            } else if batch {
                batch_label.as_str()
            } else {
                "▶ Run pipeline"
            };
            let run_text = if run_enabled {
                egui::RichText::new(run_label)
                    .strong()
                    .color(egui::Color32::WHITE)
            } else {
                egui::RichText::new(run_label).strong()
            };
            let mut run_btn = egui::Button::new(run_text).min_size(egui::vec2(unit * 6.5, btn_h));
            if run_enabled {
                run_btn = run_btn.fill(pal.accent_fill).stroke(egui::Stroke::NONE);
            }
            let run_response = ui.add_enabled(run_enabled, run_btn);
            self.run_button_id = Some(run_response.id);
            if ui.memory(|m| m.has_focus(run_response.id)) {
                ui.painter().rect_stroke(
                    run_response.rect.expand(2.0),
                    ui.visuals().widgets.active.corner_radius,
                    egui::Stroke::new(2.0_f32, pal.info),
                    egui::StrokeKind::Outside,
                );
            }
            let run_clicked = run_response
                .on_hover_text(if run_enabled && batch {
                    "Run the selected steps for every ticked subject, one subject after another (Ctrl+R)"
                } else if run_enabled {
                    "Start the selected steps (Ctrl+R)"
                } else if self.running {
                    "A run is already in progress"
                } else if self.batch.active {
                    crate::app::BATCH_ACTIVE
                } else {
                    self.run_wait
                        .unwrap_or("Resolve the items listed above first")
                })
                .clicked();

            let stop_label = if self.stopping {
                "⏹ Stopping…"
            } else {
                "⏹ Stop"
            };
            let stop_clicked = ui
                .add_enabled(
                    self.running && !self.stopping,
                    egui::Button::new(stop_label).min_size(egui::vec2(unit * 3.8, btn_h)),
                )
                .on_hover_text(STOP_HOVER)
                .clicked();

            if ui
                .add_enabled(
                    readiness.has_subject && readiness.has_bids_dir,
                    egui::Button::new("Show results").min_size(egui::vec2(unit * 5.8, btn_h)),
                )
                .on_hover_text("Open this subject's folder in the output directory in the file manager")
                .clicked()
            {
                *action = StagePanelAction::RevealOutputDirectory;
            }

            if stop_clicked {
                self.stopping = true;
                *action = StagePanelAction::Cancel;
            } else if run_clicked && batch {
                if let Some(req) = self.build_request(state, runnable_stages) {
                    *action = StagePanelAction::RunBatch(req);
                }
            } else if run_clicked {
                if let Some(req) = self.build_request(state, runnable_stages) {
                    let overwritten = if state.stage.overwrite_results {
                        state.stage.stages_that_would_be_overwritten(readiness)
                    } else {
                        Vec::new()
                    };
                    *action = if overwritten.is_empty() {
                        StagePanelAction::Run(req)
                    } else {
                        StagePanelAction::ConfirmOverwriteThenRun(req, overwritten)
                    };
                }
            }
        });

        if self.stopping {
            ui.label(
                egui::RichText::new("Stopping the run…")
                    .small()
                    .color(pal.warn),
            );
        }
        ui.add_space(unit * 0.2);
    }

    /// Build the run request for the given steps, or `None` when there is nothing to run.
    pub fn build_request(&self, state: &DbslocAppState, stages: &[Stage]) -> Option<RunRequest> {
        state
            .stage
            .run_request(stages, state.normalization_template_t2.as_deref())
    }
}

/// The run progress bar, coloured by outcome so it cannot contradict the
/// summary underneath it.
///
/// Fill tokens, not the foreground status colours: white text on `pal.err`
/// measures 2.4:1 in dark mode, which fails WCAG AA.
pub(crate) fn progress_bar(ui: &mut egui::Ui, progress: &RunProgress, running: bool) {
    let pal = palette(ui);
    let fill = if progress.any_failed() {
        pal.err_fill
    } else if progress.any_cancelled() || progress.any_rejected() {
        pal.warn_fill
    } else {
        pal.accent_fill
    };
    ui.horizontal(|ui| {
        if running {
            let size = em(ui);
            ui.add(egui::Spinner::new().size(size));
        }
        let width = ui.available_width();
        ui.add(
            egui::ProgressBar::new(progress.fraction())
                .desired_width(width)
                .desired_height(em(ui) * 0.95)
                .fill(fill)
                .text(egui::RichText::new(progress.progress_text()).color(egui::Color32::WHITE)),
        )
        .on_hover_text("Steps finished in this run, the outcome, and elapsed time");
    });
}

pub(crate) fn labelled_value(ui: &mut egui::Ui, label: &str, value: &str) {
    let pal = palette(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("{label}:")).color(pal.text_muted));
        ui.label(value);
    });
}

const STIMULATION_NEEDS_RECONSTRUCTION: &str = "Run Reconstruct first";

fn stimulation_summary(labels: &[String]) -> (String, String) {
    if labels.is_empty() {
        return (
            "No stimulation settings yet".to_owned(),
            "Nothing is saved for this subject yet. Edit stimulation… creates the first one"
                .to_owned(),
        );
    }
    let full = labels.join(", ");
    (format!("Stimulations: {full}"), full)
}

pub(crate) fn vat_card_body(
    ui: &mut egui::Ui,
    state: &DbslocAppState,
    readiness: &PipelineReadiness,
    pal: &tokens::Palette,
) -> NormalizeBrowse {
    let mut browse = NormalizeBrowse::None;
    let (text, hover) = stimulation_summary(&readiness.stimulation_labels);
    ui.add(
        egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted))
            .truncate()
            .selectable(false),
    )
    .on_hover_text(hover);
    let has_reconstruction = readiness.has_reconstruction_json;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(has_reconstruction, egui::Button::new("Edit stimulation…"))
            .on_hover_text(
                "Edit the stimulation settings of this subject, preview the volume of activated tissue and compute it",
            )
            .on_disabled_hover_text(STIMULATION_NEEDS_RECONSTRUCTION)
            .clicked()
        {
            browse = NormalizeBrowse::Stimulation;
        }
        if ui
            .add_enabled(
                has_reconstruction,
                egui::Button::new("Import session report…"),
            )
            .on_hover_text(
                "Read the stimulation settings of a vendor session report (PDF) into this subject",
            )
            .on_disabled_hover_text(STIMULATION_NEEDS_RECONSTRUCTION)
            .clicked()
        {
            crate::report_import::request_import(ui.ctx());
        }
    });
    crate::report_import::reports_card_section(ui, state);
    browse
}

/// The options shown inside one step card, shared by the Pipeline tab and the
/// guided run. Returns the template file the user asked to browse for, if any.
pub(crate) fn stage_options(
    stage: Stage,
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    pal: &tokens::Palette,
    mode: CardMode,
    readiness: &PipelineReadiness,
) -> NormalizeBrowse {
    let mut browse = NormalizeBrowse::None;
    match stage {
        Stage::Preprocess | Stage::Coregister => {
            labelled_value(
                ui,
                "Method",
                &registration_method_label(stage, state.stage.engine_for(stage)),
            );
            engine_row(ui, state, stage);
            if stage == Stage::Coregister {
                postop_modality_row(ui, state, readiness);
            }
        }
        Stage::Normalize => browse = normalize_options(ui, state, readiness),
        Stage::Reconstruct => {
            reconstruct_options(ui, state, pal);
            if mode == CardMode::Selectable {
                ui.checkbox(
                    &mut state.stage.open_viewer_after_reconstruct,
                    "Open the 3D viewer when reconstruction finishes",
                )
                .on_hover_text(
                    "Switch to the 3D Viewer tab and load this subject's scene automatically",
                );
            }
        }
        Stage::Orientation => {
            ui.horizontal_wrapped(|ui| {
                labelled_value(
                    ui,
                    "Method",
                    "Experimental: estimates the roll of Boston and Abbott directional leads from the CT",
                );
                info_icon(
                    ui,
                    "Directional orientation (specs/06) is experimental. It reads the post-op CT and the reconstruction file and records the roll of each directional lead in the reconstruction file. Other lead models are left unresolved.",
                );
            });
        }
        Stage::Vat => {}
        Stage::Visualize => {
            labelled_value(
                ui,
                "Output",
                "3D scene manifest in the subject reconstruction folder",
            );
            atlases_dir_row(ui, state);
        }
        Stage::Bids | Stage::Precoreg | Stage::Brainshift => {}
    }
    browse
}

pub(crate) const POSTOP_MODALITY_HOVER: &str = "Used only when the subject has both a post-op      CT and a post-op MRI: which of them localizes the leads. A subject with only one of them      always uses that one. Automatic takes the CT unless the subject's Lead-DBS settings choose      the MRI.";

pub(crate) fn postop_modality_row(
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    readiness: &PipelineReadiness,
) {
    let pal = palette(ui);
    let Some(subject) = state
        .selected_subject
        .clone()
        .filter(|s| !s.trim().is_empty())
    else {
        return;
    };
    let only = match (readiness.has_raw_postop_ct, readiness.has_raw_postop_mr) {
        (true, true) => None,
        (true, false) => Some("CT"),
        (false, true) => Some("MRI"),
        (false, false) => return,
    };
    if let Some(kind) = only {
        ui.label(
            egui::RichText::new(format!("{subject} has only a post-op {kind}"))
                .color(pal.text_muted),
        );
        postop_selected_rows(ui, readiness, state.stage.postop_modality, &pal);
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Post-op images to use:");
        egui::ComboBox::from_id_salt("postop_modality")
            .selected_text(state.stage.postop_modality.label())
            .show_ui(ui, |ui| {
                for m in dbsloc_ui::PostopModality::ALL {
                    ui.selectable_value(&mut state.stage.postop_modality, m, m.label());
                }
            })
            .response
            .on_hover_text(POSTOP_MODALITY_HOVER);
    });
    postop_selected_rows(ui, readiness, state.stage.postop_modality, &pal);
}

fn postop_selected_rows(
    ui: &mut egui::Ui,
    readiness: &PipelineReadiness,
    choice: dbsloc_ui::PostopModality,
    pal: &tokens::Palette,
) {
    for row in readiness.postop_images_for(choice) {
        image_row_line(ui, row, pal);
    }
}

const ATLASES_DIR_INFO: &str = "A Lead-DBS installation folder or its atlases directory. Every \
     atlas folder in it is listed in the 3D Viewer's Atlas set list beside the bundled atlases; \
     the folders are read where they are and never copied.";

/// The *Lead-DBS atlases directory* setting of the Visualize step.
fn atlases_dir_row(ui: &mut egui::Ui, state: &mut DbslocAppState) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        ui.label("Lead-DBS atlases directory:");
        match state.stage.leaddbs_atlases_dir.as_ref() {
            Some(dir) => {
                let text = dir.display().to_string();
                ui.add(egui::Label::new(text.as_str()).truncate())
                    .on_hover_text(text);
            }
            None => {
                ui.label(egui::RichText::new("none, bundled atlases only").color(pal.text_muted));
            }
        }
        info_icon(ui, ATLASES_DIR_INFO);
    });
    ui.horizontal_wrapped(|ui| {
        let picking = state.atlas_dir_pick.is_open();
        if ui
            .add_enabled(!picking, egui::Button::new("Choose directory…"))
            .on_hover_text("Choose the folder that holds the Lead-DBS atlases")
            .clicked()
        {
            let start = state.stage.leaddbs_atlases_dir.clone();
            state
                .atlas_dir_pick
                .open(ui.ctx(), "Select the Lead-DBS atlases directory", start);
        }
        if ui
            .button("Import from Lead-DBS…")
            .on_hover_text(
                "Use or copy the atlases and connectomes of an existing Lead-DBS installation",
            )
            .clicked()
        {
            state.leaddbs_import_request = true;
        }
        if ui
            .button("Download atlases…")
            .on_hover_text(
                "Download single Lead-DBS atlases from the Lead-DBS server to this computer",
            )
            .clicked()
        {
            crate::atlas_download::open_window(ui.ctx());
        }
        if state.stage.leaddbs_atlases_dir.is_some()
            && ui
                .button("Clear")
                .on_hover_text("Show only the bundled atlases")
                .clicked()
        {
            state.stage.leaddbs_atlases_dir = None;
        }
    });
    if let Some(name) = crate::atlas_download::running_download(ui.ctx()) {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("Downloading the atlas {name}…"))
                    .small()
                    .color(pal.text_muted),
            )
            .wrap(),
        );
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "A subject's atlas list changes on its next Reconstruct or Visualize run.",
            )
            .small()
            .color(pal.text_muted),
        )
        .wrap(),
    );
}

/// Which template file the Normalize options asked the application to browse for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalizeBrowse {
    None,
    Template,
    TemplateT2,
    Stimulation,
}

/// The template combo box with its download controls and, for a custom file,
/// the path field. Returns `true` when the user asked to browse for a file.
fn template_picker_row(ui: &mut egui::Ui, state: &mut DbslocAppState) -> bool {
    use crate::template_picker::{matching_entry, row_label, size_label};
    let unit = em(ui);
    let pal = palette(ui);
    let mut browse = false;
    let current = state.normalization_template.clone().unwrap_or_default();
    let labels: Vec<String> = state.template_ui.rows().iter().map(row_label).collect();
    let matched = matching_entry(state.template_ui.rows(), &current);
    let pending = state.template_ui.pending();
    let custom_shown =
        state.template_ui.custom_requested() || (matched.is_none() && pending.is_none());
    let downloading = state.template_ui.is_downloading();
    let selected_text = match (pending, matched) {
        (Some(i), _) => labels.get(i).cloned().unwrap_or_default(),
        (None, Some(i)) if !state.template_ui.custom_requested() => {
            labels.get(i).cloned().unwrap_or_default()
        }
        _ => "Custom file…".to_owned(),
    };
    let mut chosen: Option<usize> = None;
    let mut chose_custom = false;
    ui.horizontal(|ui| {
        ui.label("Template:");
        ui.add_enabled_ui(!downloading, |ui| {
            egui::ComboBox::from_id_salt("norm_template_entry")
                .width((ui.available_width() - unit).max(unit * 8.0))
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for (i, label) in labels.iter().enumerate() {
                        if ui
                            .selectable_label(pending.or(matched) == Some(i), label)
                            .clicked()
                        {
                            chosen = Some(i);
                        }
                    }
                    if ui.selectable_label(custom_shown, "Custom file…").clicked() {
                        chose_custom = true;
                    }
                })
                .response
                .on_hover_text(
                    "The template the anatomy is warped into. Bundled and installed templates are ready to use; others are downloaded once and checked against a published checksum.",
                );
        });
    });
    if let Some(i) = chosen {
        let st = &mut *state;
        st.template_ui.choose(
            i,
            &mut st.normalization_template,
            &mut st.normalization_template_t2,
        );
    }
    if chose_custom {
        state.template_ui.choose_custom();
        browse = true;
    }
    if let Some(progress) = state.template_ui.progress() {
        let frac = if progress.total > 0 {
            (progress.downloaded as f32 / progress.total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let text = format!(
            "Downloading: {} of {}",
            size_label(progress.downloaded),
            size_label(progress.total)
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(text)
                    .desired_width((ui.available_width() - unit * 6.0).max(unit * 6.0)),
            );
            if ui
                .button("Cancel")
                .on_hover_text("Stop the download and discard the partial file")
                .clicked()
            {
                state.template_ui.cancel();
            }
        });
    } else if let Some(i) = state.template_ui.pending() {
        let size = match state.template_ui.rows().get(i).map(|r| &r.state) {
            Some(dbsloc_pipeline::template_fetch::EntryState::Available { size }) => Some(*size),
            _ => None,
        };
        if let Some(size) = size {
            let ctx = ui.ctx().clone();
            if ui
                .button(format!("Download ({})", size_label(size)))
                .on_hover_text(
                    "Download this template once into your user data folder. The file is checked against a published checksum before it is installed.",
                )
                .clicked()
            {
                state.template_ui.start_download(i, &ctx);
            }
        }
    }
    if let Some(err) = state.template_ui.error() {
        ui.label(egui::RichText::new(format!("Download failed: {err}")).color(pal.err));
    }
    if custom_shown {
        ui.horizontal(|ui| {
            let mut tmpl = current;
            let w = (ui.available_width() - unit * 3.0).max(unit * 6.0);
            let resp = ui
                .add(
                    egui::TextEdit::singleline(&mut tmpl)
                        .hint_text("Path to a template T1 NIfTI file")
                        .desired_width(w),
                )
                .on_hover_text("Path to the T1 template NIfTI file the anatomy is warped into");
            if resp.changed() {
                state.normalization_template = if tmpl.trim().is_empty() {
                    None
                } else {
                    Some(tmpl)
                };
            }
            if ui
                .button("…")
                .on_hover_text("Browse for the T1 template NIfTI file")
                .clicked()
            {
                browse = true;
            }
        });
    }
    browse
}

const TEMPLATE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

type ResolvedGridKey = (u8, u64, RegBackend, usize, bool);

#[derive(Default)]
pub struct TemplateGridCache {
    key: Option<String>,
    checked: Option<std::time::Instant>,
    path: Option<PathBuf>,
    voxel: Option<f64>,
    dims: Option<(Option<u64>, Option<[usize; 3]>)>,
    resolved: Option<(ResolvedGridKey, ResolvedNormGrid)>,
}

impl TemplateGridCache {
    pub fn voxel_mm(&mut self, raw: Option<&str>) -> Option<f64> {
        let key = raw.map(str::to_owned);
        let stale = self.key != key
            || (self.voxel.is_none() && self.checked.is_none_or(|t| t.elapsed() >= TEMPLATE_RETRY));
        if stale {
            let resolved =
                dbsloc_pipeline::templates::select_template(raw.map(Path::new)).map(|r| r.path);
            self.voxel = resolved
                .as_deref()
                .and_then(dbsloc_pipeline::template_voxel_mm);
            self.path = resolved;
            self.key = key;
            self.checked = Some(std::time::Instant::now());
            self.dims = None;
            self.resolved = None;
        }
        self.voxel
    }

    /// The grid the pipeline would register on for `grid` with the `backend` engine, with the
    /// capacity step of the default against the start-up probe's device limits (none while the
    /// probe still runs or with ANTs).
    pub fn resolved(
        &mut self,
        grid: NormGrid,
        backend: RegBackend,
        channels: usize,
    ) -> ResolvedNormGrid {
        let caps = dbsloc_pipeline::reg_backend::planning_capacity_caps_if_ready(backend);
        let (kind, bits) = match grid {
            NormGrid::Template => (0, 0),
            NormGrid::Full => (1, 0),
            NormGrid::Mm(v) => (2, v.to_bits()),
        };
        let key = (kind, bits, backend, channels, caps.is_some());
        if let Some((k, r)) = &self.resolved {
            if *k == key {
                return r.clone();
            }
        }
        let r = dbsloc_pipeline::norm_grid::resolve_for_template(
            grid,
            self.path.as_deref(),
            channels,
            caps.as_ref(),
        );
        self.resolved = Some((key, r.clone()));
        r
    }

    pub fn grid_dims(&mut self, iso_mm: Option<f64>) -> Option<[usize; 3]> {
        let bits = iso_mm.map(f64::to_bits);
        if let Some((k, dims)) = self.dims {
            if k == bits {
                return dims;
            }
        }
        let dims = self
            .path
            .as_deref()
            .and_then(|p| dbsloc_pipeline::norm_grid_dims(p, iso_mm));
        self.dims = Some((bits, dims));
        dims
    }
}

/// Template paths and registration resolution. Returns the file the user asked
/// to browse for — the picker is opened by the application, off this thread.
fn normalize_options(
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    readiness: &PipelineReadiness,
) -> NormalizeBrowse {
    let unit = em(ui);
    let mut browse = NormalizeBrowse::None;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Method:").color(palette(ui).text_muted));
        ui.label("Symmetric normalization (SyN)");
        info_icon(
            ui,
            "Symmetric diffeomorphic normalization (SyN) warps the anatomy into template space.",
        );
    });
    if let Some(row) = readiness.anchor_image() {
        ui.horizontal(|ui| {
            let unit = em(ui);
            ui.label(egui::RichText::new("Anchor:").color(palette(ui).text_muted));
            let room = (ui.available_width() - unit * 4.5).max(unit * 4.0);
            ui.add_sized(
                [room, unit * 1.3],
                egui::Label::new(image_file_name(&row.path)).truncate(),
            )
            .on_hover_text(format!(
                "The pre-op image this step registers to the template: {}",
                row.path.display()
            ));
            image_view_button(ui, row);
        });
    }
    if template_picker_row(ui, state) {
        browse = NormalizeBrowse::Template;
    }
    ui.horizontal(|ui| {
        ui.label("T2 template:");
        let mut tmpl = state.normalization_template_t2.clone().unwrap_or_default();
        let w = (ui.available_width() - unit * 3.0).max(unit * 6.0);
        let resp = ui
            .add(
                egui::TextEdit::singleline(&mut tmpl)
                    .hint_text("leave empty to use the T1 template's T2")
                    .desired_width(w),
            )
            .on_hover_text(
                "Path to the T2 template used by the multispectral registration. When empty, \
                 a T2 template beside the T1 template is used if one exists.",
            );
        if resp.changed() {
            state.normalization_template_t2 = if tmpl.trim().is_empty() {
                None
            } else {
                Some(tmpl)
            };
        }
        if ui
            .button("…")
            .on_hover_text("Browse for the T2 template NIfTI file")
            .clicked()
        {
            browse = NormalizeBrowse::TemplateT2;
        }
    });
    ui.checkbox(
        &mut state.stage.norm_multispectral,
        "Multispectral (T1w + T2w)",
    )
    .on_hover_text(
        "Register the T1w and a T2w against the template together when the subject has both and \
         a T2 template is available. Untick for T1w only.",
    );
    let voxel = state
        .norm_grid
        .voxel_mm(state.normalization_template.as_deref());
    ui.horizontal(|ui| {
        ui.label("Resolution:");
        egui::ComboBox::from_id_salt("norm_resolution")
            .selected_text(state.stage.norm_resolution.label_for(voxel))
            .show_ui(ui, |ui| {
                for v in [
                    NormResolution::Template,
                    NormResolution::Mm1p0,
                    NormResolution::Mm0p8,
                    NormResolution::Mm0p5,
                    NormResolution::FullTemplate,
                    NormResolution::Custom,
                ] {
                    let label = v.label_for(voxel);
                    match voxel.filter(|_| v.finer_than(voxel)) {
                        Some(vox) => {
                            ui.add_enabled(false, egui::Button::selectable(false, label))
                                .on_disabled_hover_text(finer_disabled_hover(vox));
                        }
                        None => {
                            ui.selectable_value(&mut state.stage.norm_resolution, v, label)
                                .on_hover_text(v.hint());
                        }
                    }
                }
            })
            .response
            .on_hover_text(
                "Isotropic voxel size, in millimetres, the anatomy and the template are \
                 resampled to before registration. The normalized CT always keeps the \
                 template grid. Memory and time grow with the number of voxels shown below.",
            );
        if state.stage.norm_resolution == NormResolution::Custom {
            let response = ui
                .add(
                    egui::DragValue::new(&mut state.stage.norm_custom_mm)
                        .range(norm_custom_range(voxel).0..=norm_custom_range(voxel).1)
                        .clamp_existing_to_range(false)
                        .speed(0.01)
                        .fixed_decimals(2)
                        .suffix(" mm"),
                )
                .on_hover_text("Isotropic voxel size of the registration grid, in millimetres");
            if response.changed() {
                state.stage.norm_custom_mm = if state.stage.norm_custom_mm.is_finite() {
                    (state.stage.norm_custom_mm * 100.0).round() / 100.0
                } else {
                    1.5
                };
            }
        }
    });
    let grid = state.stage.norm_grid();
    if let Some(vox) = voxel.filter(|&v| grid.mm().is_some_and(|mm| finer_than_voxel(mm, v))) {
        ui.label(egui::RichText::new(finer_selection_warning(vox)).color(palette(ui).warn));
    }
    let channels = if state.stage.norm_multispectral { 2 } else { 1 };
    let resolved = state
        .norm_grid
        .resolved(grid, state.stage.normalize_engine.value(), channels);
    if let (Some(from), Some(to), Some(reason)) = (
        resolved.template_mm,
        resolved.mm,
        resolved.reason.as_deref(),
    ) {
        ui.add(
            egui::Label::new(
                egui::RichText::new(stepped_grid_text(from, to)).color(palette(ui).warn),
            )
            .wrap(),
        )
        .on_hover_text(format!(
            "{reason}. Pick a resolution above to use another grid; an explicit choice is never changed."
        ));
    }
    if voxel.is_some() {
        if let Some(dims) = state.norm_grid.grid_dims(resolved.mm) {
            ui.label(egui::RichText::new(grid_size_text(dims)).color(palette(ui).text_muted));
        }
    }
    engine_row(ui, state, Stage::Normalize);
    browse
}

fn engine_slot(settings: &mut StageSettings, stage: Stage) -> Option<&mut RegistrationEngine> {
    match stage {
        Stage::Preprocess => Some(&mut settings.preprocess_engine),
        Stage::Coregister => Some(&mut settings.coregister_engine),
        Stage::Normalize => Some(&mut settings.normalize_engine),
        _ => None,
    }
}

/// The *Engine* combo of one registration step.
pub(crate) fn engine_row(ui: &mut egui::Ui, state: &mut DbslocAppState, stage: Stage) {
    let Some(engine) = engine_slot(&mut state.stage, stage) else {
        return;
    };
    let step = match stage {
        Stage::Preprocess => "Preprocess",
        Stage::Coregister => "Coregister",
        _ => "Normalize",
    };
    ui.horizontal(|ui| {
        ui.label("Engine:");
        egui::ComboBox::from_id_salt(("registration_engine", step))
            .selected_text(engine.label())
            .show_ui(ui, |ui| {
                for v in [
                    RegistrationEngine::Auto,
                    RegistrationEngine::Gpu,
                    RegistrationEngine::Ants,
                ] {
                    ui.selectable_value(engine, v, v.label())
                        .on_hover_text(v.hint());
                }
            })
            .response
            .on_hover_text(engine.hint());
        info_icon(ui, &engine_info_text(step));
    });
}

fn engine_info_text(step: &str) -> String {
    format!(
        "Which engine runs the registration in {step}; the other steps keep their own \
         choice. Auto uses the graphics card when a hardware adapter passes the \
         start-up self-test, otherwise ANTs on the processor."
    )
}

/// The thread limit, graphics card status and driver notice shared by **Preprocess, Coregister
/// and Normalize**.
pub(crate) fn registration_shared_options(
    ui: &mut egui::Ui,
    state: &mut DbslocAppState,
    gpu_help: &crate::gpu_status::GpuHelp,
) {
    threads_row(ui, &mut state.stage.ants_threads);
    gpu_status_line(ui);
    crate::gpu_status::notice(ui, gpu_help);
}

pub(crate) fn review_mode_row(ui: &mut egui::Ui, mode: &mut ReviewMode) {
    ui.horizontal(|ui| {
        ui.label(format!("{REVIEW_MODE_LABEL}:"));
        egui::ComboBox::from_id_salt("review_mode")
            .selected_text(mode.label())
            .show_ui(ui, |ui| {
                for m in ReviewMode::ALL {
                    ui.selectable_value(mode, m, m.label())
                        .on_hover_text(m.hint());
                }
            })
            .response
            .on_hover_text(REVIEW_MODE_HOVER);
    });
}

fn available_threads() -> u32 {
    static MAX: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
            .max(1)
    })
}

/// The text a thread-count slider shows for `value` out of `max`.
fn threads_text(value: u32, max: u32) -> String {
    if value >= max {
        format!("{max} (all)")
    } else {
        value.to_string()
    }
}

/// The thread limit slider: the right end is every core and stores `None`.
fn threads_row(ui: &mut egui::Ui, cap: &mut Option<u32>) {
    let max = available_threads();
    let mut value = cap.map_or(max, |n| n.clamp(1, max));
    let track = em(ui) * 10.0;
    ui.horizontal(|ui| {
        ui.label("Number of threads:");
        ui.spacing_mut().slider_width = track;
        let rail = palette(ui).hairline_strong;
        ui.visuals_mut().widgets.inactive.bg_fill = rail;
        let response = ui
            .add(
                egui::Slider::new(&mut value, 1..=max)
                    .trailing_fill(true)
                    .custom_formatter(move |v, _| threads_text(v as u32, max))
                    .custom_parser(|text| {
                        let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
                        digits.parse::<f64>().ok()
                    }),
            )
            .on_hover_text(format!(
                "Maximum number of processor threads Preprocess, Coregister and Normalize may use. Default: all {max}."
            ));
        if response.changed() {
            *cap = if value >= max { None } else { Some(value) };
        }
    });
}

/// The detected graphics card and its self-test verdict. The check runs once in the background
/// at start-up; until it finishes the line says so.
fn gpu_status_line(ui: &mut egui::Ui) {
    let pal = palette(ui);
    let (text, colour) = match dbsloc_pipeline::reg_backend::cached_probe_if_ready() {
        None => ("Checking for a graphics card…".to_owned(), pal.text_muted),
        Some(p) if p.available => (p.status_line(), pal.text_muted),
        Some(p) => (p.status_line(), pal.warn),
    };
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(colour)).wrap())
        .on_hover_text(
            "Adapter and result of the start-up self-test the GPU engine runs on known data. \
         Software adapters are never used unless DBSLOC_GPU_ALLOW_SOFTWARE=1 is set.",
        );
}

const PACER_NOT_IN_BUILD: &str = "PaCER is not included in this build of Wyss DBS Localizer. \
     PaCER is an optional component licensed under the GNU Affero General Public License \
     (AGPL-3.0-only), and this copy was built without it. \
     Choose Automatic or TRAC / CORE, or install the standard build, which includes PaCER.";

pub(crate) const PACER_SWITCHED_NOTE: &str = "The saved reconstruction method PaCER is not \
     included in this build of Wyss DBS Localizer, so the method was set to Automatic.";

fn recon_method_available(v: ReconMethod) -> bool {
    v != ReconMethod::Pacer || dbsloc_pipeline::PACER_AVAILABLE
}

pub(crate) fn drop_unavailable_recon_method(method: &mut ReconMethod) -> bool {
    if recon_method_available(*method) {
        return false;
    }
    *method = ReconMethod::Auto;
    true
}

fn recon_method_label(v: ReconMethod) -> String {
    if recon_method_available(v) {
        v.label().to_owned()
    } else {
        format!("{} (not in this build)", v.label())
    }
}

fn recon_method_hint(v: ReconMethod) -> &'static str {
    if recon_method_available(v) {
        v.hint()
    } else {
        PACER_NOT_IN_BUILD
    }
}

fn reconstruct_options(ui: &mut egui::Ui, state: &mut DbslocAppState, pal: &tokens::Palette) {
    let unit = em(ui);
    ui.horizontal(|ui| {
        ui.label("Method:");
        let current = state.stage.recon_method;
        egui::ComboBox::from_id_salt("recon_method")
            .selected_text(recon_method_label(current))
            .show_ui(ui, |ui| {
                for v in [
                    ReconMethod::Auto,
                    ReconMethod::TracCore,
                    ReconMethod::TracCoreNative,
                    ReconMethod::Pacer,
                ] {
                    let available = recon_method_available(v);
                    ui.add_enabled_ui(available, |ui| {
                        ui.selectable_value(&mut state.stage.recon_method, v, recon_method_label(v))
                    })
                    .inner
                    .on_hover_text(v.hint())
                    .on_disabled_hover_text(recon_method_hint(v));
                }
            })
            .response
            .on_hover_text(recon_method_hint(current));
        info_icon(
            ui,
            "Which algorithm locates the lead. PaCER and the native TRAC / CORE path need a \
             post-operative CT; a post-operative MRI uses template-space TRAC / CORE.",
        );
    });
    ui.horizontal(|ui| {
        ui.label("Electrode model:");
        let selected = if state.stage.el_model_from_subject {
            EL_MODEL_FROM_SUBJECT_LABEL.to_owned()
        } else {
            state
                .stage
                .el_model
                .clone()
                .unwrap_or_else(|| NO_ELECTRODE_LABEL.to_owned())
        };
        egui::ComboBox::from_id_salt("el_model")
            .selected_text(selected)
            .width((ui.available_width() - unit * 0.5).max(unit * 8.0))
            .show_ui(ui, |ui| {
                let stage = &mut state.stage;
                if ui
                    .selectable_label(
                        !stage.el_model_from_subject && stage.el_model.is_none(),
                        NO_ELECTRODE_LABEL,
                    )
                    .clicked()
                {
                    stage.el_model_from_subject = false;
                    stage.el_model = None;
                }
                if ui
                    .selectable_label(stage.el_model_from_subject, EL_MODEL_FROM_SUBJECT_LABEL)
                    .on_hover_text(EL_MODEL_FROM_SUBJECT_HOVER)
                    .clicked()
                {
                    stage.el_model_from_subject = true;
                }
                for name in dbsloc_pipeline::electrode_model_names() {
                    let on =
                        !stage.el_model_from_subject && stage.el_model.as_deref() == Some(name);
                    if ui.selectable_label(on, name).clicked() {
                        stage.el_model_from_subject = false;
                        stage.el_model = Some(name.to_owned());
                    }
                }
            })
            .response
            .on_hover_text(
                "The implanted lead. Contact spacing and the plausibility tolerances come \
                 from this model.",
            );
    });
    if state.stage.el_model.is_none() && !state.stage.el_model_from_subject {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "⚠  Reconstruction needs an electrode model — contact spacing and the \
                     plausibility tolerances come from it.",
                )
                .color(pal.warn),
            )
            .wrap(),
        );
    }
    ui.horizontal(|ui| {
        ui.label("Metal appearance:");
        egui::ComboBox::from_id_salt("metal_contrast")
            .selected_text(state.stage.metal_contrast.label())
            .show_ui(ui, |ui| {
                for v in [MetalContrast::Dark, MetalContrast::Bright] {
                    ui.selectable_value(&mut state.stage.metal_contrast, v, v.label())
                        .on_hover_text(v.hint());
                }
            })
            .response
            .on_hover_text(state.stage.metal_contrast.hint());
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardAction {
    None,
    ReviewNow,
    ReviewStage(Stage),
    RerunStep,
    RefineLocalization,
    OpenFolder,
}

pub(crate) fn card_to_panel_action(card: CardAction, stage: Stage) -> StagePanelAction {
    match card {
        CardAction::None => StagePanelAction::None,
        CardAction::ReviewNow => StagePanelAction::ReviewNow,
        CardAction::ReviewStage(target) => StagePanelAction::ReviewStage(target),
        CardAction::RerunStep => StagePanelAction::RerunStep(stage),
        CardAction::RefineLocalization => StagePanelAction::RefineLocalization,
        CardAction::OpenFolder => StagePanelAction::OpenStageFolder(stage),
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CardReview {
    pub chip: Option<RegistrationReview>,
    pub can_review: bool,
    pub preparing: Option<Stage>,
    pub warnings: Vec<UpstreamReviewWarning>,
    pub refine: CardRefine,
    pub reco_sides: Vec<RecoSideSummary>,
    pub output_folder: bool,
}

pub(crate) fn stage_output_folder(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Preprocess => Some("preprocessing"),
        Stage::Coregister => Some("coregistration"),
        Stage::Normalize => Some("normalization"),
        Stage::Reconstruct | Stage::Orientation => Some("reconstruction"),
        Stage::Vat => Some("stimulations"),
        _ => None,
    }
}

pub(crate) const OPEN_FOLDER_LABEL: &str = "Open folder";

pub(crate) fn open_folder_button(ui: &mut egui::Ui, hover: &str) -> bool {
    ui.small_button(OPEN_FOLDER_LABEL)
        .on_hover_text(hover)
        .clicked()
}

pub(crate) fn stage_folder_hover(stage: Stage) -> &'static str {
    match stage {
        Stage::Preprocess => "Open the folder with the preprocessed images in the file manager",
        Stage::Coregister => {
            "Open the folder with the coregistered images and transforms in the file manager"
        }
        Stage::Normalize => {
            "Open the folder with the normalized images and the normalization transforms (affine .mat and warp fields) in the file manager"
        }
        Stage::Reconstruct | Stage::Orientation => {
            "Open the folder with the electrode reconstruction (.json and the Lead-DBS compatible .mat) in the file manager"
        }
        Stage::Vat => {
            "Open the folder with the stimulation settings and the volumes of tissue activated in the file manager"
        }
        _ => "Open the folder in the file manager",
    }
}

pub(crate) fn shorten_path(path: &Path, max_chars: usize) -> String {
    let full = path.display().to_string();
    if full.chars().count() <= max_chars {
        return full;
    }
    let keep = max_chars.saturating_sub(1).max(1);
    let tail: String = full.chars().skip(full.chars().count() - keep).collect();
    let tail = match tail.find(['\\', '/']) {
        Some(i) if i + 1 < tail.len() => tail[i..].to_owned(),
        _ => tail,
    };
    format!("…{tail}")
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefineActivity {
    #[default]
    Idle,
    Preparing,
    Saving,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CardRefine {
    pub shown: bool,
    pub enabled: bool,
    pub activity: RefineActivity,
    pub corrected: bool,
    pub rerun_replaces: bool,
}

pub(crate) fn card_review(
    readiness: &PipelineReadiness,
    settings: &StageSettings,
    stage: Stage,
    running: bool,
    preparing: Option<Stage>,
    refine_activity: RefineActivity,
) -> CardReview {
    let refine_shown = stage == Stage::Reconstruct && readiness.has_reconstruction_json;
    let reco_sides = if refine_shown {
        readiness.reco_sides.clone()
    } else {
        Vec::new()
    };
    let reviewed = readiness.reco_manually_corrected
        || reco_sides
            .iter()
            .any(|s| matches!(s.review, LeadReview::Corrected | LeadReview::Accepted));
    let refine = CardRefine {
        shown: refine_shown,
        enabled: refine_shown && !running && refine_activity == RefineActivity::Idle,
        activity: refine_activity,
        corrected: refine_shown && readiness.reco_manually_corrected,
        rerun_replaces: refine_shown
            && reviewed
            && settings.overwrite_results
            && settings.selected(Stage::Reconstruct),
    };
    let chip = readiness
        .registration_review(stage)
        .filter(|_| readiness.stage_output_exists(stage));
    CardReview {
        chip,
        can_review: chip.is_some() && !running,
        preparing,
        warnings: settings.upstream_review_warnings(readiness, stage),
        refine,
        reco_sides,
        output_folder: stage_output_folder(stage).is_some() && readiness.stage_output_exists(stage),
    }
}

pub(crate) const REVIEW_BUTTON_LABEL: &str = "Review registration";
pub(crate) const REVIEW_BUTTON_HOVER: &str =
    "Show the registration images of this step and approve or reject them.";
pub(crate) const REVIEW_PREPARING_TEXT: &str = "Preparing the registration images…";

fn review_button(
    ui: &mut egui::Ui,
    target: Stage,
    enabled: bool,
    preparing: Option<Stage>,
    action: &mut CardAction,
) {
    if preparing == Some(target) {
        let size = em(ui);
        ui.add(egui::Spinner::new().size(size));
        let pal = palette(ui);
        ui.add(
            egui::Label::new(
                egui::RichText::new(REVIEW_PREPARING_TEXT)
                    .small()
                    .color(pal.text_muted),
            )
            .selectable(false),
        );
        return;
    }
    if ui
        .add_enabled(
            enabled && preparing.is_none(),
            egui::Button::new(REVIEW_BUTTON_LABEL),
        )
        .on_hover_text(REVIEW_BUTTON_HOVER)
        .clicked()
    {
        *action = CardAction::ReviewStage(target);
    }
}

pub(crate) const REFINE_BUTTON_LABEL: &str = "Refine localization";
pub(crate) const REFINE_BUTTON_HOVER: &str = "Inspect the lead on the post-op image, move the head and tail fiducials and set the roll; contacts follow the electrode model";
pub(crate) const REFINE_PREPARING_TEXT: &str = "Opening the post-op image…";
pub(crate) const REFINE_SAVING_TEXT: &str = "Saving the corrected lead…";
pub(crate) const REFINE_BUSY_HOVER: &str =
    "Refine localization is busy. Runs and other registration reviews wait until it has finished.";
pub(crate) const CORRECTED_CHIP_TEXT: &str = "Manually corrected";
pub(crate) const CORRECTED_CHIP_HOVER: &str = "At least one lead was corrected by hand in Refine localization. Later steps use the corrected lead.";
pub(crate) const RERUN_REPLACES_TEXT: &str =
    "Rerun all selected steps traces every lead again and drops the manual corrections and acceptances";

fn refine_row(
    ui: &mut egui::Ui,
    refine: &CardRefine,
    sides: &[RecoSideSummary],
    pal: &tokens::Palette,
    action: &mut CardAction,
) {
    if !sides.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for side in sides {
                let color = match side.review {
                    LeadReview::Corrected | LeadReview::Accepted => pal.info,
                    LeadReview::Unchecked => pal.text_muted,
                    LeadReview::NotFound => pal.warn,
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(side.text()).small().color(color))
                        .selectable(false),
                )
                .on_hover_text(side.hover());
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        if refine.corrected {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("●  {CORRECTED_CHIP_TEXT}")).color(pal.info),
                )
                .selectable(false),
            )
            .on_hover_text(CORRECTED_CHIP_HOVER);
        }
        let busy_text = match refine.activity {
            RefineActivity::Idle => None,
            RefineActivity::Preparing => Some(REFINE_PREPARING_TEXT),
            RefineActivity::Saving => Some(REFINE_SAVING_TEXT),
        };
        if let Some(text) = busy_text {
            let size = em(ui);
            ui.add(egui::Spinner::new().size(size))
                .on_hover_text(REFINE_BUSY_HOVER);
            ui.add(
                egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted))
                    .selectable(false),
            )
            .on_hover_text(REFINE_BUSY_HOVER);
            return;
        }
        if ui
            .add_enabled(refine.enabled, egui::Button::new(REFINE_BUTTON_LABEL))
            .on_hover_text(REFINE_BUTTON_HOVER)
            .clicked()
        {
            *action = CardAction::RefineLocalization;
        }
    });
    if refine.rerun_replaces {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("⚠  {RERUN_REPLACES_TEXT}")).color(pal.warn),
            )
            .wrap()
            .selectable(false),
        );
    }
}

fn review_chip(ui: &mut egui::Ui, state: RegistrationReview, pal: &tokens::Palette) {
    let color = match state {
        RegistrationReview::Approved => pal.ok,
        RegistrationReview::Rejected => pal.warn,
        RegistrationReview::NotReviewed => pal.text_muted,
    };
    ui.add(
        egui::Label::new(egui::RichText::new(state.chip_text()).small().color(color))
            .selectable(false),
    )
    .on_hover_text(state.chip_hover());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardPart {
    Body,
    Options,
}

/// How a step card presents its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardMode {
    /// A checkbox picks the step for the next run (Pipeline tab).
    Selectable,
    /// The card stands for one guided step: a plain title, no checkbox.
    Guided { open_options: bool },
}

/// One pipeline step card.
///
/// The **Options** open/closed state is owned by egui (persisted through
/// eframe), so it is deliberately not duplicated in the application state.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stage_card(
    ui: &mut egui::Ui,
    meta: &StageMeta,
    status: StageState,
    status_text: &str,
    run_status: Option<StageRunStatus>,
    skip_reason: Option<&str>,
    enabled: bool,
    will_skip: bool,
    review: &CardReview,
    pal: &tokens::Palette,
    selected: &mut bool,
    mode: CardMode,
    mut options: impl FnMut(&mut egui::Ui, CardPart),
) -> CardAction {
    let unit = em(ui);
    let is_selected = *selected;
    let mut card_action = CardAction::None;

    egui::Frame::group(ui.style())
        .fill(if is_selected {
            pal.surface_raised
        } else {
            pal.surface
        })
        .stroke(egui::Stroke::new(
            1.0_f32,
            if is_selected {
                pal.hairline_strong
            } else {
                pal.hairline
            },
        ))
        .show(ui, |ui| {
            let mut header = |ui: &mut egui::Ui, folded: bool| {
                match mode {
                    CardMode::Selectable => {
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.checkbox(selected, egui::RichText::new(meta.title).strong())
                                .on_hover_text(meta.description);
                        });
                    }
                    CardMode::Guided { .. } => {
                        ui.add(
                            egui::Label::new(egui::RichText::new(meta.title).strong())
                                .selectable(false),
                        );
                    }
                }
                info_icon(ui, meta.description);
                if folded {
                    let (glyph, color) = match status {
                        StageState::Ready => ("✔", pal.ok),
                        StageState::NeedsUpstream => ("⚠", pal.warn),
                        StageState::Blocked => ("✖", pal.err),
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(glyph).color(color))
                            .selectable(false),
                    )
                    .on_hover_text(status_text);
                }
                if let Some(rs) = run_status {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (glyph, text, color) = match rs {
                            StageRunStatus::Pending => ("○", "queued", pal.text_muted),
                            StageRunStatus::Running => ("▶", "running", pal.info),
                            StageRunStatus::Ok => ("✔", "done", pal.ok),
                            StageRunStatus::Skipped => ("–", "skipped", pal.text_muted),
                            StageRunStatus::Cancelled => ("⏹", "cancelled", pal.warn),
                            StageRunStatus::Failed => ("✖", "failed", pal.err),
                            StageRunStatus::AwaitingReview => {
                                ("◔", "awaiting review", pal.info)
                            }
                            StageRunStatus::Rejected => ("⚠", "needs rework", pal.warn),
                            StageRunStatus::Deferred => ("◔", "awaiting review", pal.info),
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("{glyph} {text}"))
                                    .small()
                                    .color(color),
                            )
                            .selectable(false),
                        )
                        .on_hover_text(match (rs, skip_reason) {
                            (StageRunStatus::Skipped, Some(reason)) => {
                                format!("In the most recent run, this step was skipped: {reason}.")
                            }
                            (StageRunStatus::AwaitingReview, _) => "The run is waiting for you to approve or reject this step's registration.".to_owned(),
                            (StageRunStatus::Rejected, _) => "You rejected this registration and the run stopped. Rerun the step to redo it.".to_owned(),
                            (StageRunStatus::Deferred, _) => "This registration waits for review. The subject's later steps run after you approve it; the reviews open when every subject has had its turn.".to_owned(),
                            _ => format!("In the most recent run, this step {text}."),
                        });
                    });
                }
            };
            let mut body = |ui: &mut egui::Ui| {
            let (glyph, color) = match status {
                StageState::Ready => ("✔", pal.ok),
                StageState::NeedsUpstream => ("⚠", pal.warn),
                StageState::Blocked => ("✖", pal.err),
            };
            ui.horizontal_wrapped(|ui| {
                status_chip(ui, glyph, status_text, color);
                if review.output_folder
                    && open_folder_button(ui, stage_folder_hover(meta.stage))
                {
                    card_action = CardAction::OpenFolder;
                }
            });
            if run_status == Some(StageRunStatus::AwaitingReview)
                && ui
                    .button("Review now")
                    .on_hover_text(
                        "Open the registration review to approve or reject this step (Ctrl+5)",
                    )
                    .clicked()
            {
                card_action = CardAction::ReviewNow;
            }
            if run_status == Some(StageRunStatus::Rejected)
                && ui
                    .add_enabled(enabled, egui::Button::new("Rerun step"))
                    .on_hover_text(
                        "Run only this step again and replace its output; you are asked to confirm first",
                    )
                    .clicked()
            {
                card_action = CardAction::RerunStep;
            }
            if let Some(state) = review.chip {
                ui.horizontal_wrapped(|ui| {
                    review_chip(ui, state, pal);
                    if review.can_review || review.preparing == Some(meta.stage) {
                        review_button(
                            ui,
                            meta.stage,
                            review.can_review,
                            review.preparing,
                            &mut card_action,
                        );
                    }
                });
            }
            if review.refine.shown {
                refine_row(
                    ui,
                    &review.refine,
                    &review.reco_sides,
                    pal,
                    &mut card_action,
                );
            }
            for warning in &review.warnings {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("⚠  {}", warning.text())).color(pal.warn),
                        )
                        .wrap()
                        .selectable(false),
                    )
                    .on_hover_text(warning.hover());
                    review_button(
                        ui,
                        warning.upstream,
                        enabled,
                        review.preparing,
                        &mut card_action,
                    );
                });
            }
            if let (Some(StageRunStatus::Skipped), Some(reason)) = (run_status, skip_reason) {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("Skipped: {reason}"))
                            .small()
                            .color(pal.text_muted),
                    )
                    .wrap()
                    .selectable(false),
                );
            }
            if will_skip {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("–  Will be skipped: output already exists")
                            .color(pal.text_muted),
                    )
                    .wrap()
                    .selectable(false),
                )
                .on_hover_text(
                    "This step is ticked, but its output is already on disk, so the run will pass over it. Tick \"Rerun all selected steps\" to run it again.",
                );
            }

            ui.add_enabled_ui(enabled, |ui| options(ui, CardPart::Body));

            if is_selected && meta.stage != Stage::Vat {
                ui.add_space(unit * 0.2);
                let (salt, open) = match mode {
                    CardMode::Selectable => (meta.id.to_owned(), false),
                    CardMode::Guided { open_options } => (format!("{}_guided", meta.id), open_options),
                };
                egui::CollapsingHeader::new("Options")
                    .id_salt(salt)
                    .default_open(open)
                    .show(ui, |ui| {
                        ui.add_enabled_ui(enabled, |ui| options(ui, CardPart::Options));
                    });
            }
                    };
            ui.set_width(ui.available_width());
            match mode {
                CardMode::Selectable => {
                    let id = egui::Id::new(("step_card", meta.id));
                    let fold = egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        id,
                        true,
                    );
                    let folded = !fold.is_open();
                    fold.show_header(ui, |ui| header(ui, folded)).body(|ui| body(ui));
                }
                CardMode::Guided { .. } => {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| header(ui, false));
                    body(ui);
                }
            }
        });
    ui.add_space(unit * 0.35);
    card_action
}

fn popout_title_id() -> egui::Id {
    egui::Id::new("popout_window_title")
}

pub(crate) fn open_popout_title(ctx: &egui::Context) -> Option<String> {
    let now = ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT);
    ctx.data(|d| d.get_temp::<(u64, String)>(popout_title_id()))
        .filter(|(frame, _)| now.saturating_sub(*frame) <= 2)
        .map(|(_, title)| title)
}

pub(crate) struct PopoutBar {
    what: String,
    own_window: bool,
    fullscreen: bool,
    toggle_fullscreen: bool,
    bring_back: bool,
}

impl PopoutBar {
    const FULLSCREEN_ID: &'static str = "popout_fullscreen";
    const DOCK_ID: &'static str = "popout_dock";

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        if self.own_window
            && ui
                .button(self.fullscreen_label())
                .on_hover_text(self.fullscreen_hover())
                .clicked()
        {
            self.toggle_fullscreen = true;
        }
        if ui
            .button("Dock to main window")
            .on_hover_text(self.dock_hover())
            .clicked()
        {
            self.bring_back = true;
        }
    }

    pub(crate) fn host_buttons(&self) -> Vec<dbsloc_viz::HostButton> {
        let mut buttons = Vec::new();
        if self.own_window {
            buttons.push(dbsloc_viz::HostButton {
                id: Self::FULLSCREEN_ID,
                label: self.fullscreen_label().to_owned(),
                hover: self.fullscreen_hover(),
                primary: false,
            });
        }
        buttons.push(dbsloc_viz::HostButton {
            id: Self::DOCK_ID,
            label: "Dock to main window".to_owned(),
            hover: self.dock_hover(),
            primary: false,
        });
        buttons
    }

    pub(crate) fn apply_host_click(&mut self, id: Option<&'static str>) {
        match id {
            Some(Self::FULLSCREEN_ID) => self.toggle_fullscreen = true,
            Some(Self::DOCK_ID) => self.bring_back = true,
            _ => {}
        }
    }

    fn fullscreen_label(&self) -> &'static str {
        if self.fullscreen {
            "Exit full screen"
        } else {
            "Full screen"
        }
    }

    fn fullscreen_hover(&self) -> String {
        if self.fullscreen {
            "Leave full screen (F11 or Esc)".to_owned()
        } else {
            format!("Fill the whole screen with {} (F11)", self.what)
        }
    }

    fn dock_hover(&self) -> String {
        format!(
            "Close the separate window and show {} in the main window again",
            self.what
        )
    }
}

pub(crate) fn popout_window(
    ctx: &egui::Context,
    viewport_salt: &str,
    window_title: &str,
    what: &str,
    inset: bool,
    mut body: impl FnMut(&mut egui::Ui, &mut PopoutBar),
) -> bool {
    let builder = egui::ViewportBuilder::default()
        .with_title(window_title)
        .with_icon(dbsloc_viz::brand::window_icon())
        .with_inner_size([1280.0, 860.0])
        .with_min_inner_size([720.0, 520.0]);
    let builder = if crate::app::screenshot_mode() {
        builder
            .with_inner_size([1100.0, 700.0])
            .with_position([10.0, 10.0])
    } else {
        builder
    };
    let mut bring_back = false;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of(viewport_salt),
        builder,
        |ui, class| {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| i.viewport().close_requested()) {
                bring_back = true;
                return;
            }
            let own_window = class == egui::ViewportClass::Immediate;
            let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            let mut bar = PopoutBar {
                what: what.to_owned(),
                own_window,
                fullscreen,
                toggle_fullscreen: own_window
                    && ctx.input_mut(|i| {
                        i.consume_shortcut(&egui::KeyboardShortcut::new(
                            egui::Modifiers::NONE,
                            egui::Key::F11,
                        ))
                    }),
                bring_back: false,
            };
            if own_window
                && fullscreen
                && ctx.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(
                        egui::Modifiers::NONE,
                        egui::Key::Escape,
                    ))
                })
            {
                bar.toggle_fullscreen = true;
            }
            let margin = if inset {
                egui::Margin::same((em(ui) * 0.5).round() as i8)
            } else {
                egui::Margin::ZERO
            };
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .inner_margin(margin),
                )
                .show(ui, |ui| body(ui, &mut bar));
            let frame_nr = ctx.cumulative_frame_nr_for(egui::ViewportId::ROOT);
            ctx.data_mut(|d| d.insert_temp(popout_title_id(), (frame_nr, window_title.to_owned())));
            if bar.bring_back {
                bring_back = true;
            }
            if bar.toggle_fullscreen {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
        },
    );
    bring_back
}

/// Hard cap on retained log lines. Older lines are dropped in blocks, so trimming
/// stays amortized-constant instead of shifting the buffer on every push.
const LOG_CAPACITY: usize = 20_000;
const LOG_TRIM_BLOCK: usize = 2_000;
/// How many lines wrapped mode renders. Wrapped rows have varying heights and
/// so cannot be virtualized; this keeps the frame bounded.
const WRAP_RENDER_CAP: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogKind {
    Plain,
    Started,
    Ok,
    Skipped,
    Failed,
    Warning,
    Summary,
}

impl LogKind {
    fn is_problem(self) -> bool {
        matches!(self, LogKind::Failed | LogKind::Warning)
    }
}

/// Which stream a raw pipeline line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogStream {
    /// Produced by the application or the step machinery itself.
    Internal,
    Stdout,
    Stderr,
}

struct LogLine {
    kind: LogKind,
    stream: LogStream,
    /// Text as stored: the stream marker is kept, so Debug can show it and the
    /// other detail levels can strip it. Nothing is ever discarded on ingest.
    raw: String,
    lower: String,
}

impl LogLine {
    fn display(&self, detail: OutputDetail) -> &str {
        match detail {
            OutputDetail::Debug => &self.raw,
            _ => strip_stream_prefix(&self.raw),
        }
    }

    fn visible_at(&self, detail: OutputDetail) -> bool {
        match detail {
            OutputDetail::Debug | OutputDetail::Detailed => true,
            OutputDetail::Standard => self.stream != LogStream::Stdout,
        }
    }
}

#[derive(Default)]
pub struct LogPanel {
    lines: VecDeque<LogLine>,
    /// Indices into `lines` that pass the current filters — the virtual row list.
    visible: Vec<usize>,
    filter: String,
    /// `filter`, lowercased once — `line_matches` runs per ingested line.
    filter_lower: String,
    problems_only: bool,
    detail: OutputDetail,
    dropped: usize,
    rejected_stage: Option<String>,
    rework: u32,
    deferred: u32,
    /// `[sub-xx]` before each pipeline line while a run of several subjects is going.
    pub prefix: Option<String>,
    /// A registration gate that is not approved means "awaiting review", not "rejected".
    pub defer_review: bool,
    pub event_rx: Option<mpsc::Receiver<RunEvent>>,
    completed_seen: bool,
    ended_unexpectedly: bool,
}

impl LogPanel {
    pub fn attach(&mut self, rx: mpsc::Receiver<RunEvent>) {
        self.event_rx = Some(rx);
        self.completed_seen = false;
        self.ended_unexpectedly = false;
    }

    pub fn take_unexpected_end(&mut self) -> bool {
        std::mem::take(&mut self.ended_unexpectedly)
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.visible.clear();
        self.dropped = 0;
    }

    pub fn copy_all(&self, ctx: &egui::Context) {
        let text: String = self
            .visible
            .iter()
            .filter_map(|i| self.lines.get(*i))
            .map(|l| l.display(self.detail))
            .collect::<Vec<_>>()
            .join("\n");
        ctx.copy_text(text);
    }

    /// Drain pending events from the pipeline runner. Returns them so the caller
    /// can advance run progress; the log only keeps the rendered text.
    pub fn poll_events(&mut self, stage_panel: &mut StagePanel) -> Vec<RunEvent> {
        let mut events = Vec::new();
        let Some(rx) = self.event_rx.take() else {
            return events;
        };
        let mut keep_rx = true;
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    let line = match &ev {
                        RunEvent::StageStarted { stage } => {
                            Some((LogKind::Started, format!("▶ {stage}")))
                        }
                        RunEvent::StageCompleted { stage, .. } => {
                            Some((LogKind::Ok, format!("✔ {stage}")))
                        }
                        RunEvent::StageSkipped { stage, reason, .. } => Some((
                            LogKind::Skipped,
                            format!("– {stage} skipped: {}", humanize_message(reason)),
                        )),
                        RunEvent::StageFailed { stage, message } => {
                            if self.rejected_stage.as_deref() == Some(stage.as_str()) {
                                self.rejected_stage = None;
                                None
                            } else {
                                Some((
                                    LogKind::Failed,
                                    format!("✖ {stage}: {}", humanize_message(message)),
                                ))
                            }
                        }
                        RunEvent::StageAwaitingQc { stage, .. } if self.defer_review => {
                            self.rejected_stage = Some(stage.clone());
                            self.deferred += 1;
                            Some((
                                LogKind::Skipped,
                                format!(
                                    "◔ {stage}: registration awaits review — this subject's later steps run after approval"
                                ),
                            ))
                        }
                        RunEvent::StageAwaitingQc { stage, .. } => {
                            self.rejected_stage = Some(stage.clone());
                            self.rework += 1;
                            Some((
                                LogKind::Warning,
                                format!("⚠ {stage}: registration rejected — run stopped"),
                            ))
                        }
                        RunEvent::LogLine { line } => Some((LogKind::Plain, line.clone())),
                        RunEvent::PipelineCompleted {
                            stages_ok,
                            stages_failed,
                            stages_skipped,
                        } => {
                            stage_panel.running = false;
                            stage_panel.stopping = false;
                            self.completed_seen = true;
                            let ended = format!(
                                "run ended: {stages_ok} ok, {stages_failed} failed, {stages_skipped} skipped"
                            );
                            if *stages_failed > 0 {
                                crate::app_log::warn(&ended);
                            } else {
                                crate::app_log::info(&ended);
                            }
                            let rework = std::mem::take(&mut self.rework);
                            let deferred = std::mem::take(&mut self.deferred);
                            self.rejected_stage = None;
                            let verb = if rework > 0 {
                                "stopped"
                            } else if deferred > 0 {
                                "paused"
                            } else {
                                "finished"
                            };
                            let waiting = if deferred > 0 {
                                format!(", {deferred} awaiting review")
                            } else {
                                String::new()
                            };
                            Some((
                                LogKind::Summary,
                                format!(
                                    "Pipeline {verb}: {}{waiting}",
                                    crate::app::run_summary(
                                        *stages_ok,
                                        stages_failed.saturating_sub(rework + deferred),
                                        *stages_skipped,
                                        rework,
                                    )
                                ),
                            ))
                        }
                    };
                    if let Some((kind, line)) = line {
                        let line = match &self.prefix {
                            Some(p) => format!("[{p}] {line}"),
                            None => line,
                        };
                        self.push_line(kind, line);
                    }
                    events.push(ev);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    stage_panel.running = false;
                    stage_panel.stopping = false;
                    self.ended_unexpectedly = !self.completed_seen;
                    keep_rx = false;
                    break;
                }
            }
        }
        if keep_rx {
            self.event_rx = Some(rx);
        }
        events
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.push_line(LogKind::Plain, line.into());
    }

    pub fn push_error(&mut self, line: impl Into<String>) {
        self.push_line(LogKind::Failed, line.into());
    }

    fn push_line(&mut self, kind: LogKind, raw: String) {
        if self.lines.len() >= LOG_CAPACITY {
            self.trim(LOG_TRIM_BLOCK);
        }
        let raw = humanize_message(&raw);
        crate::app_log::log(
            match kind {
                LogKind::Failed => crate::app_log::Level::Error,
                LogKind::Warning => crate::app_log::Level::Warn,
                _ => crate::app_log::level_from_line(&raw),
            },
            &raw,
        );
        let stream = if raw.starts_with("[out]") {
            LogStream::Stdout
        } else if raw.starts_with("[err]") {
            LogStream::Stderr
        } else {
            LogStream::Internal
        };
        let lower = raw.to_ascii_lowercase();
        let idx = self.lines.len();
        let line = LogLine {
            kind,
            stream,
            raw,
            lower,
        };
        let matches = self.line_matches(&line);
        self.lines.push_back(line);
        if matches {
            self.visible.push(idx);
        }
    }

    fn trim(&mut self, count: usize) {
        let count = count.min(self.lines.len());
        self.lines.drain(..count);
        self.dropped += count;
        self.visible.retain(|i| *i >= count);
        for i in &mut self.visible {
            *i -= count;
        }
    }

    fn line_matches(&self, line: &LogLine) -> bool {
        if !line.visible_at(self.detail) {
            return false;
        }
        if self.problems_only && !line.kind.is_problem() {
            return false;
        }
        self.filter_lower.is_empty() || line.lower.contains(&self.filter_lower)
    }

    fn rebuild_visible(&mut self) {
        self.filter_lower = self.filter.trim().to_ascii_lowercase();
        let needle = self.filter_lower.clone();
        let problems_only = self.problems_only;
        let detail = self.detail;
        self.visible = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                l.visible_at(detail)
                    && (!problems_only || l.kind.is_problem())
                    && (needle.is_empty() || l.lower.contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        detail: &mut OutputDetail,
        autoscroll: &mut bool,
        wrap: &mut bool,
    ) {
        let unit = em(ui);
        let pal = palette(ui);
        let mut refilter = false;

        if self.detail != *detail {
            self.detail = *detail;
            refilter = true;
        }

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.lines.is_empty(), egui::Button::new("Clear"))
                    .on_hover_text("Remove all output shown here (Ctrl+L)")
                    .clicked()
                {
                    self.clear();
                }
                if ui
                    .add_enabled(!self.lines.is_empty(), egui::Button::new("Copy"))
                    .on_hover_text("Copy the visible output to the clipboard (Ctrl+Shift+C)")
                    .clicked()
                {
                    self.copy_all(ctx);
                }
                ui.separator();
                ui.checkbox(autoscroll, "Follow output")
                    .on_hover_text("Keep scrolling to the newest line as output arrives");
                ui.checkbox(wrap, "Wrap lines").on_hover_text(
                    "Wrap long lines instead of scrolling sideways. Error messages carry \
                     the part that says what to do at the end of the line, so this is worth \
                     turning on to read a failure.",
                );
                let before = *detail;
                egui::ComboBox::from_id_salt("output_detail")
                    .selected_text(detail.label())
                    .show_ui(ui, |ui| {
                        for v in [
                            OutputDetail::Standard,
                            OutputDetail::Detailed,
                            OutputDetail::Debug,
                        ] {
                            ui.selectable_value(detail, v, v.label())
                                .on_hover_text(v.hint());
                        }
                    })
                    .response
                    .on_hover_text(
                        "How much of the captured output is shown. Every line stays stored, \
                         so raising the detail always brings text back.",
                    );
                ui.label("Detail:");
                ui.separator();
                if *detail != before {
                    self.detail = *detail;
                    refilter = true;
                }
                if ui
                    .checkbox(&mut self.problems_only, "Failures only")
                    .on_hover_text("Show only lines reporting a failed step")
                    .changed()
                {
                    refilter = true;
                }
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label("🔍").on_hover_text("Filter the output by text");
                    if ui
                        .add_sized(
                            [ui.available_width(), ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(&mut self.filter).hint_text("Filter output"),
                        )
                        .on_hover_text("Show only lines containing this text")
                        .changed()
                    {
                        refilter = true;
                    }
                });
            });
        });
        if refilter {
            self.rebuild_visible();
        }

        let capped = *wrap && self.visible.len() > WRAP_RENDER_CAP;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{} of {} lines{}",
                    self.visible.len(),
                    self.lines.len(),
                    if self.dropped > 0 {
                        format!(" · {} earlier lines dropped", self.dropped)
                    } else {
                        String::new()
                    }
                ))
                .small()
                .color(pal.text_muted),
            );
            if capped {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "· showing the last {WRAP_RENDER_CAP} while Wrap lines is on"
                        ))
                        .small()
                        .color(pal.warn),
                    )
                    .selectable(false),
                )
                .on_hover_text(
                    "Wrapped rows are not a uniform height, so they cannot be virtualized. \
                     Turn Wrap lines off to scroll the whole log, or use Copy — Copy is \
                     never capped.",
                );
            }
        });
        ui.separator();

        if self.lines.is_empty() {
            ui.add_space(unit * 2.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No output yet.").color(pal.text_muted));
                ui.label(
                    egui::RichText::new("Start a run with ▶ Run pipeline, or press Ctrl+R.")
                        .small()
                        .color(pal.text_muted),
                );
            });
            return;
        }
        if self.visible.is_empty() {
            ui.add_space(unit * 2.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No line matches the current filter.")
                        .color(pal.text_muted),
                );
            });
            return;
        }

        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        let rows = self.visible.len();
        let detail = self.detail;
        let wrap = *wrap;
        let margin_x = (unit * 0.6) as i8;
        let text_width = (ui.available_width()
            - 2.0 * margin_x as f32
            - ui.spacing().scroll.bar_width
            - ui.spacing().scroll.bar_inner_margin)
            .max(unit * 4.0);
        egui::Frame::NONE
            .fill(pal.canvas)
            .inner_margin(egui::Margin::symmetric(margin_x, (unit * 0.4) as i8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let color_of = |line: &LogLine| match line.kind {
                    LogKind::Plain => {
                        if line.stream == LogStream::Stderr {
                            pal.text_muted
                        } else {
                            pal.text
                        }
                    }
                    LogKind::Started => pal.info,
                    LogKind::Ok => pal.ok,
                    LogKind::Skipped => pal.text_muted,
                    LogKind::Failed => pal.err,
                    LogKind::Warning => pal.warn,
                    LogKind::Summary => pal.text,
                };
                if wrap {
                    let first = rows.saturating_sub(WRAP_RENDER_CAP);
                    egui::ScrollArea::vertical()
                        .id_salt("output_log_wrapped")
                        .stick_to_bottom(*autoscroll)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mono = egui::TextStyle::Monospace.resolve(ui.style());
                            let gutter = ui
                                .painter()
                                .layout_no_wrap("  ".to_owned(), mono.clone(), pal.text)
                                .size()
                                .x;
                            let body_w = (text_width - gutter).max(1.0);
                            for row in first..rows {
                                let Some(line) =
                                    self.visible.get(row).and_then(|i| self.lines.get(*i))
                                else {
                                    continue;
                                };
                                let text = line.display(detail);
                                let (marker, body) = match text.char_indices().nth(2) {
                                    Some((i, _)) if text.starts_with(['▶', '✔', '✖', '–']) => {
                                        (&text[..i], &text[i..])
                                    }
                                    _ => ("", text),
                                };
                                let body_color = if line.kind == LogKind::Failed {
                                    pal.text
                                } else {
                                    color_of(line)
                                };
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(gutter, 0.0),
                                        egui::Layout::left_to_right(egui::Align::TOP),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(marker)
                                                    .monospace()
                                                    .color(color_of(line)),
                                            );
                                        },
                                    );
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(body_w, 0.0),
                                        egui::Layout::top_down(egui::Align::LEFT),
                                        |ui| {
                                            let mut job = egui::text::LayoutJob::simple(
                                                body.to_owned(),
                                                mono.clone(),
                                                body_color,
                                                body_w,
                                            );
                                            job.wrap.break_anywhere = true;
                                            ui.add(egui::Label::new(job));
                                        },
                                    );
                                });
                            }
                        });
                } else {
                    egui::ScrollArea::both()
                        .id_salt("output_log")
                        .stick_to_bottom(*autoscroll)
                        .auto_shrink([false, false])
                        .show_rows(ui, row_h, rows, |ui, range| {
                            for row in range {
                                let Some(line) =
                                    self.visible.get(row).and_then(|i| self.lines.get(*i))
                                else {
                                    continue;
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(line.display(detail))
                                            .monospace()
                                            .color(color_of(line)),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            }
                        });
                }
            });
    }
}

/// Make a message from the pipeline fit to show a person.
///
/// Two machine artefacts leak into otherwise good error text: Windows
/// extended-length path prefixes (`\\?\C:\…`), and an error kind repeated by
/// nested `Display` implementations (`BIDS error: BIDS error: …`). Both are
/// noise at the point of reading; the underlying text is otherwise verbatim.
pub fn humanize_message(text: &str) -> String {
    let mut out = text.replace("\\\\?\\UNC\\", "\\\\").replace("\\\\?\\", "");
    if let Some((head, _)) = out.split_once(": ") {
        if !head.is_empty() && head.len() <= 32 && !head.contains('\\') {
            let prefix = format!("{head}: ");
            let keep = prefix.len();
            let rest = out[keep..].replace(&prefix, "");
            out = format!("{prefix}{rest}");
        }
    }
    out
}

fn strip_stream_prefix(line: &str) -> &str {
    line.strip_prefix("[out] ")
        .or_else(|| line.strip_prefix("[err] "))
        .unwrap_or(line)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenePanelAction {
    None,
    BrowseManifest,
    BrowseBackdrop,
}

#[derive(Debug, Clone)]
struct BackdropSource {
    space: String,
    options: Vec<BackdropOption>,
}

fn backdrop_kind(name: &str) -> BackdropKind {
    match name {
        "template" => BackdropKind::Template,
        "download" => BackdropKind::Download,
        "custom" => BackdropKind::Custom,
        _ => BackdropKind::Patient,
    }
}

fn same_subject(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_start_matches("sub-").to_owned();
    norm(a) == norm(b)
}

fn resolve_backdrop_options(
    bids_root: &str,
    output_dir: Option<&Path>,
    subject: Option<&str>,
    manifest: &SceneManifestV1,
) -> Vec<BackdropOption> {
    let resolved = subject
        .filter(|s| !s.is_empty() && !bids_root.is_empty())
        .and_then(|s| {
            dbsloc_pipeline::bids::resolve_subject_in(Path::new(bids_root), output_dir, s, true)
                .ok()
        })
        .filter(|sub| same_subject(&sub.subj_id, &manifest.subj_id));
    let options = match resolved {
        Some(sub) => dbsloc_pipeline::backdrops::scene_backdrops(
            &sub,
            &manifest.template_nii,
            &manifest.space,
        ),
        None => {
            let mut options: Vec<BackdropOption> = manifest
                .backdrops
                .iter()
                .filter(|b| b.path.is_file())
                .map(|b| BackdropOption {
                    id: b.id.clone(),
                    label: b.label.clone(),
                    path: b.path.clone(),
                    kind: backdrop_kind(&b.kind),
                })
                .collect();
            if options.is_empty() && manifest.template_nii.is_file() {
                options.push(BackdropOption {
                    id: "template-t1".to_owned(),
                    label: format!("{} T1w (template)", manifest.space),
                    path: manifest.template_nii.clone(),
                    kind: BackdropKind::Template,
                });
            }
            options
        }
    };
    options
        .into_iter()
        .filter(|o| o.kind != BackdropKind::Download)
        .collect()
}

fn source_choices(src: &BackdropSource, persisted_id: Option<&str>) -> Vec<BackdropChoice> {
    let entries = dbsloc_pipeline::backdrop_catalog::entries_for_space(&src.space);
    backdrop_choices(&src.options, &entries, persisted_id)
}

type LoadedScene = (String, Scene3DPayload, BackdropSource, Vec<BackdropChoice>);
type ManifestLoadMsg = (u64, Result<LoadedScene, String>);
type ManifestLoadRx = mpsc::Receiver<ManifestLoadMsg>;

/// Persisted 3D-viewer state: the manifest path and the camera / atlas setup.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneSettings {
    pub manifest_path: String,
    /// True once the user picked a manifest by hand; the default path then stops
    /// overwriting their choice until the subject or data directory changes, or
    /// the chosen file no longer exists at start-up.
    pub manifest_path_is_manual: bool,
    pub view: Option<SceneViewState>,
}

#[derive(Default)]
pub struct ScenePanel {
    embedded: Option<Scene3DEframeApp>,
    loaded_manifest: Option<PathBuf>,
    loading_manifest: Option<PathBuf>,
    last_error: Option<String>,
    loading: bool,
    load_generation: u64,
    pending: Option<ManifestLoadRx>,
    tracked_bids: String,
    tracked_output: Option<PathBuf>,
    tracked_subject: Option<String>,
    tracked_any: bool,
    /// Manifest path an automatic load has already been attempted for, so a
    /// failing path is not retried on every frame.
    auto_attempted: Option<String>,
    /// View state waiting to be applied to a scene that is still loading.
    restore_on_load: Option<SceneViewState>,
    /// The scene-source section has already been folded away once.
    source_open_seen: bool,
    popped_out: bool,
    navigation: NavigationScheme,
    backdrop_source: Option<BackdropSource>,
    custom_backdrop: Option<String>,
    pending_backdrop_select: Option<String>,
    backdrop_dl: BackdropDownloadUi,
    want_browse_backdrop: bool,
    fibre_layers: Vec<std::sync::Arc<dbsloc_viz::scene_layers::FibreLayer>>,
    map_layers: Vec<std::sync::Arc<dbsloc_viz::scene_layers::MapLayer>>,
}

/// A folder dialog on a worker thread; the result is taken with [`FolderPick::poll`].
#[derive(Default)]
pub struct FolderPick {
    rx: Option<mpsc::Receiver<Option<PathBuf>>>,
}

impl FolderPick {
    pub fn is_open(&self) -> bool {
        self.rx.is_some()
    }

    pub fn open(&mut self, ctx: &egui::Context, title: &'static str, start: Option<PathBuf>) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self) -> Option<PathBuf> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(pick) => {
                self.rx = None;
                pick
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }
}

fn default_manifest_path(state: &DbslocAppState, subject_folder: &str) -> String {
    state
        .subject_derivatives_dir(subject_folder)
        .map(|d| {
            d.join("reconstruction")
                .join("viz")
                .join("scene_manifest.json")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default()
}

impl ScenePanel {
    pub fn has_scene(&self) -> bool {
        self.embedded.is_some()
    }

    /// Identity of the loaded scene, which changes when another scene is loaded.
    pub fn scene_key(&self) -> Option<usize> {
        self.embedded
            .as_ref()
            .map(|app| std::sync::Arc::as_ptr(&app.payload) as usize)
    }

    /// Loads a scene manifest chosen by the program, such as a group scene, in place of the
    /// subject's own.
    pub fn load_manifest_file(
        &mut self,
        state: &mut DbslocAppState,
        ctx: &egui::Context,
        path: PathBuf,
    ) {
        state.scene.manifest_path = path.to_string_lossy().into_owned();
        state.scene.manifest_path_is_manual = true;
        self.embedded = None;
        self.auto_attempted = Some(state.scene.manifest_path.trim().to_owned());
        let restore = state.scene.view.clone();
        self.start_load(path, ctx, restore);
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Whether the loaded scene is a group scene.
    pub fn has_group_scene(&self) -> bool {
        self.embedded
            .as_ref()
            .is_some_and(|app| app.payload.group().is_some())
    }

    /// Applies the display flags and the highlighted patient or group of the group panel to the
    /// loaded group scene. Returns `false` when no group scene is loaded.
    pub fn apply_group_look(&self, look: &crate::group_panel::GroupLook) -> bool {
        let Some(app) = &self.embedded else {
            return false;
        };
        let Some(mut display) = app.payload.group_display() else {
            return false;
        };
        display.show_active = look.show_active;
        display.show_passive = look.show_passive;
        display.highlight_active = look.highlight_active;
        display.selected_patient = look.selected_patient.clone();
        display.selected_group = look.selected_group;
        app.payload.set_group_display(display);
        true
    }

    /// Replaces the volumes of tissue activated drawn in the loaded scene.
    pub fn set_vta_surfaces(&self, surfaces: Vec<dbsloc_viz::vta_mesh::VtaSurface>) {
        if let Some(app) = &self.embedded {
            app.payload.set_vta_surfaces(surfaces);
        }
    }

    pub fn upsert_fibre_layer(&mut self, layer: dbsloc_viz::scene_layers::FibreLayer) {
        let layer = std::sync::Arc::new(layer);
        match self.fibre_layers.iter_mut().find(|l| l.id == layer.id) {
            Some(slot) => *slot = std::sync::Arc::clone(&layer),
            None => self.fibre_layers.push(std::sync::Arc::clone(&layer)),
        }
        if let Some(app) = &self.embedded {
            app.payload.upsert_fibre_layer_shared(layer);
        }
    }

    pub fn remove_fibre_layer(&mut self, id: &str) {
        self.fibre_layers.retain(|l| l.id != id);
        if let Some(app) = &self.embedded {
            app.payload.remove_fibre_layer(id);
        }
    }

    pub fn upsert_map_layer(&mut self, layer: dbsloc_viz::scene_layers::MapLayer) {
        let layer = std::sync::Arc::new(layer);
        match self.map_layers.iter_mut().find(|l| l.id == layer.id) {
            Some(slot) => *slot = std::sync::Arc::clone(&layer),
            None => self.map_layers.push(std::sync::Arc::clone(&layer)),
        }
        if let Some(app) = &self.embedded {
            app.payload.upsert_map_layer_shared(layer);
        }
    }

    pub fn remove_map_layer(&mut self, id: &str) {
        self.map_layers.retain(|l| l.id != id);
        if let Some(app) = &self.embedded {
            app.payload.remove_map_layer(id);
        }
    }

    fn prune_removed_layers(&mut self) {
        let Some(app) = &self.embedded else {
            return;
        };
        let (fibres, maps) = app.payload.take_removed_layers();
        self.fibre_layers.retain(|l| !fibres.contains(&l.id));
        self.map_layers.retain(|l| !maps.contains(&l.id));
    }

    pub fn take_fibre_file_request(&self) -> bool {
        self.embedded
            .as_ref()
            .is_some_and(|app| app.payload.take_fibre_file_request())
    }

    fn apply_layers(&self, payload: &Scene3DPayload) {
        payload.set_fibre_file_supported(true);
        for layer in &self.fibre_layers {
            payload.upsert_fibre_layer_shared(std::sync::Arc::clone(layer));
        }
        for layer in &self.map_layers {
            payload.upsert_map_layer_shared(std::sync::Arc::clone(layer));
        }
    }

    pub fn pop_out(&mut self) {
        self.popped_out = self.embedded.is_some();
    }

    pub fn show_popout(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.popped_out {
            return;
        }
        let Some(app) = self.embedded.as_mut() else {
            self.popped_out = false;
            ctx.request_repaint();
            return;
        };
        let backdrop_dl = &mut self.backdrop_dl;
        let bring_back = popout_window(
            ctx,
            "dbsloc_scene_popout",
            "3D Viewer - Wyss DBS Localizer",
            "the 3D Viewer",
            false,
            |ui, bar| {
                ui.push_id("backdrop_card_popout", |ui| backdrop_dl.show_card(ui, ctx));
                let clicked = app.show_embedded_with(ui, frame, bar.host_buttons());
                bar.apply_host_click(clicked);
            },
        );
        if bring_back {
            self.popped_out = false;
            ctx.request_repaint();
        }
    }

    /// Capture the live camera / atlas state so it can be persisted.
    pub fn capture_view_state(&self) -> Option<SceneViewState> {
        self.embedded.as_ref().map(Scene3DEframeApp::view_state)
    }

    fn sync_tracking(&mut self, state: &mut DbslocAppState) {
        let output = state.output_dir();
        if self.tracked_any
            && self.tracked_bids == state.bids_dir
            && self.tracked_output == output
            && self.tracked_subject == state.selected_subject
        {
            return;
        }
        let first_sync = !self.tracked_any;
        self.tracked_any = true;
        self.tracked_bids = state.bids_dir.clone();
        self.tracked_output = output;
        self.tracked_subject = state.selected_subject.clone();
        if !first_sync {
            self.embedded = None;
            self.loaded_manifest = None;
            self.loading_manifest = None;
            self.pending = None;
            self.loading = false;
            self.load_generation = self.load_generation.wrapping_add(1);
            self.last_error = None;
            self.auto_attempted = None;
            self.source_open_seen = false;
            self.backdrop_source = None;
            self.custom_backdrop = None;
            self.pending_backdrop_select = None;
            if state
                .scene
                .view
                .as_ref()
                .and_then(|v| v.backdrop_id.as_deref())
                .is_some_and(|id| id.starts_with(CUSTOM_PREFIX))
            {
                if let Some(v) = state.scene.view.as_mut() {
                    v.backdrop_id = None;
                }
            }
            self.backdrop_dl = BackdropDownloadUi::default();
            self.want_browse_backdrop = false;
            state.scene.manifest_path_is_manual = false;
        } else if state.scene.manifest_path_is_manual
            && !Path::new(state.scene.manifest_path.trim()).is_file()
        {
            state.scene.manifest_path_is_manual = false;
        }
        if state.scene.manifest_path_is_manual && !state.scene.manifest_path.trim().is_empty() {
            return;
        }
        state.scene.manifest_path = match &state.selected_subject {
            Some(sub) => default_manifest_path(state, sub),
            None => String::new(),
        };
    }

    fn refresh_backdrop_choices(&mut self) {
        let Some(src) = self.backdrop_source.clone() else {
            return;
        };
        let choices = source_choices(&src, self.custom_backdrop.as_deref());
        if let Some(app) = self.embedded.as_mut() {
            app.set_backdrop_choices(choices, true);
        }
    }

    fn service_backdrop(&mut self, ctx: &egui::Context) {
        if let Some(Outcome::Installed { id }) = self.backdrop_dl.poll() {
            if self.embedded.is_some() && !self.loading {
                self.refresh_backdrop_choices();
                if let Some(app) = self.embedded.as_mut() {
                    app.select_backdrop(&id);
                }
            } else {
                self.pending_backdrop_select = Some(id);
            }
            ctx.request_repaint();
        }
        let request = self
            .embedded
            .as_mut()
            .and_then(|app| app.take_backdrop_request());
        match request {
            Some(BackdropRequest::Install(id)) => {
                self.backdrop_dl.request(&id);
                ctx.request_repaint();
            }
            Some(BackdropRequest::ChooseFile) => {
                self.want_browse_backdrop = true;
                ctx.request_repaint();
            }
            None => {}
        }
    }

    pub fn accepts_backdrop_pick(&self) -> bool {
        self.embedded.is_some() || self.loading
    }

    pub fn add_custom_backdrop(&mut self, path: &Path) {
        if !self.accepts_backdrop_pick() {
            return;
        }
        let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let id = custom_id(&abs);
        self.custom_backdrop = Some(id.clone());
        if self.embedded.is_some() && !self.loading {
            self.refresh_backdrop_choices();
            if let Some(app) = self.embedded.as_mut() {
                app.select_backdrop(&id);
            }
        } else {
            self.pending_backdrop_select = Some(id);
        }
    }

    pub fn poll_load(&mut self, ctx: &egui::Context) {
        self.service_backdrop(ctx);
        self.prune_removed_layers();
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok((generation, res)) => {
                self.pending = None;
                self.loading = false;
                if generation != self.load_generation {
                    return;
                }
                match res {
                    Ok((sid, payload, source, choices)) => {
                        let title = format!("Scene — {sid}");
                        let saved_layers =
                            self.embedded.as_ref().map(|a| a.payload.layer_settings());
                        self.apply_layers(&payload);
                        if let Some(saved) = &saved_layers {
                            payload.restore_layer_settings(saved);
                        }
                        let mut app = Scene3DEframeApp::new(title, payload);
                        app.set_navigation(self.navigation);
                        app.set_backdrop_choices(choices, true);
                        self.backdrop_source = Some(source);
                        if let Some(view) = self.restore_on_load.take() {
                            self.custom_backdrop = view
                                .backdrop_id
                                .clone()
                                .filter(|id| id.starts_with(CUSTOM_PREFIX));
                            app.restore_view_state_deferred(&view);
                            if let Some(id) = view.atlas_id.as_deref() {
                                app.select_atlas(id);
                            }
                        }
                        self.embedded = Some(app);
                        if let Some(id) = self.pending_backdrop_select.take() {
                            if id.starts_with(CUSTOM_PREFIX) {
                                self.custom_backdrop = Some(id.clone());
                            }
                            self.refresh_backdrop_choices();
                            if let Some(app) = self.embedded.as_mut() {
                                app.select_backdrop(&id);
                            }
                        }
                        self.loaded_manifest = self.loading_manifest.clone();
                        self.last_error = None;
                    }
                    Err(e) => self.last_error = Some(e),
                }
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                self.loading = false;
            }
        }
    }

    pub fn reload_loaded_under(&mut self, folder: &Path, ctx: &egui::Context) -> bool {
        let Some(path) = self.loaded_manifest.clone() else {
            return false;
        };
        if self.embedded.is_none() || !path.starts_with(folder) || !path.is_file() {
            return false;
        }
        let restore = self.capture_view_state();
        self.start_load(path, ctx, restore);
        true
    }

    fn start_load(&mut self, path: PathBuf, ctx: &egui::Context, restore: Option<SceneViewState>) {
        self.loading_manifest = Some(path.clone());
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let (tx, rx): (mpsc::Sender<ManifestLoadMsg>, ManifestLoadRx) = mpsc::channel();
        self.pending = Some(rx);
        self.loading = true;
        self.last_error = None;
        self.restore_on_load = restore;
        let ctx_thread = ctx.clone();
        let bids = self.tracked_bids.clone();
        let output = self.tracked_output.clone();
        let subject = self.tracked_subject.clone();
        let persisted = self
            .restore_on_load
            .as_ref()
            .and_then(|v| v.backdrop_id.clone())
            .or_else(|| self.custom_backdrop.clone());
        thread::spawn(move || {
            let res: Result<LoadedScene, String> =
                SceneManifestV1::from_json_file(Path::new(&path))
                    .map_err(|e| e.to_string())
                    .and_then(|m| {
                        let sid = m.subj_id.clone();
                        let source = BackdropSource {
                            space: m.space.clone(),
                            options: resolve_backdrop_options(
                                &bids,
                                output.as_deref(),
                                subject.as_deref(),
                                &m,
                            ),
                        };
                        let choices = source_choices(&source, persisted.as_deref());
                        Scene3DPayload::from_manifest_fast(&m)
                            .map(|p| (sid, p, source, choices))
                            .map_err(|e| e.to_string())
                    });
            let _ = tx.send((generation, res));
            ctx_thread.request_repaint();
        });
        ctx.request_repaint();
    }

    pub fn load_default_manifest(
        &mut self,
        state: &mut DbslocAppState,
        ctx: &egui::Context,
        force: bool,
    ) -> Result<(), String> {
        self.sync_tracking(state);
        state.scene.manifest_path_is_manual = false;
        state.scene.manifest_path = match &state.selected_subject {
            Some(sub) => default_manifest_path(state, sub),
            None => String::new(),
        };
        let path = PathBuf::from(state.scene.manifest_path.trim());
        if path.as_os_str().is_empty() {
            return Err("No subject is selected, so there is no scene manifest path.".to_owned());
        }
        if !path.is_file() {
            return Err(format!(
                "Reconstruction finished, but no scene manifest was written at\n{}\n\n\
                 Check that the template path is valid, then review the Output tab.",
                path.display()
            ));
        }
        if !force
            && !self.loading
            && self.embedded.is_some()
            && self.loaded_manifest.as_ref() == Some(&path)
        {
            return Ok(());
        }
        self.embedded = None;
        let restore = state.scene.view.clone();
        self.start_load(path, ctx, restore);
        Ok(())
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        ctx: &egui::Context,
        state: &mut DbslocAppState,
    ) -> ScenePanelAction {
        self.sync_tracking(state);
        if self.embedded.is_none() {
            self.navigation = state.navigation;
        }
        let unit = em(ui);
        let pal = palette(ui);
        let mut action = ScenePanelAction::None;

        if self.embedded.is_some()
            && !self.popped_out
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11))
        {
            self.popped_out = true;
            ctx.request_repaint();
        }

        let manifest = state.scene.manifest_path.trim().to_owned();
        if self.embedded.is_none()
            && !self.loading
            && !manifest.is_empty()
            && self.auto_attempted.as_deref() != Some(manifest.as_str())
            && Path::new(&manifest).is_file()
        {
            self.auto_attempted = Some(manifest.clone());
            let restore = state.scene.view.clone();
            self.start_load(PathBuf::from(&manifest), ctx, restore);
        }

        let loaded = self.embedded.is_some();
        let mut source = egui::CollapsingHeader::new(if loaded {
            "Scene source"
        } else {
            "Scene source — choose a manifest to load"
        })
        .id_salt("scene_source")
        .default_open(!loaded);
        if loaded && !self.source_open_seen {
            self.source_open_seen = true;
            source = source.open(Some(false));
        }
        source.show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Scene manifest:");
                let w = (ui.available_width() - unit * 11.0).max(unit * 8.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut state.scene.manifest_path)
                            .hint_text("scene_manifest.json")
                            .desired_width(w),
                    )
                    .on_hover_text(
                        "JSON file written by the Reconstruct step, listing the template, the \
                     reconstruction and the atlas meshes to draw",
                    )
                    .changed()
                {
                    state.scene.manifest_path_is_manual = true;
                }
                if ui
                    .button("…")
                    .on_hover_text("Browse for a scene manifest JSON file")
                    .clicked()
                {
                    action = ScenePanelAction::BrowseManifest;
                }
                let can_load = !state.scene.manifest_path.trim().is_empty() && !self.loading;
                if ui
                    .add_enabled(can_load, egui::Button::new("Load scene"))
                    .on_hover_text("Load the template, the reconstruction and the atlas meshes")
                    .clicked()
                {
                    self.embedded = None;
                    self.auto_attempted = Some(state.scene.manifest_path.trim().to_owned());
                    let restore = state.scene.view.clone();
                    self.start_load(
                        PathBuf::from(state.scene.manifest_path.trim()),
                        ctx,
                        restore,
                    );
                }
                if state.scene.manifest_path_is_manual
                    && ui
                        .button("Use default path")
                        .on_hover_text("Go back to this subject's own scene manifest path")
                        .clicked()
                {
                    state.scene.manifest_path_is_manual = false;
                    state.scene.manifest_path = match &state.selected_subject {
                        Some(sub) => default_manifest_path(state, sub),
                        None => String::new(),
                    };
                }
            });
        });

        ui.push_id("backdrop_card_main", |ui| {
            self.backdrop_dl.show_card(ui, ctx)
        });
        if std::mem::take(&mut self.want_browse_backdrop) {
            action = ScenePanelAction::BrowseBackdrop;
        }
        if self.loading {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(unit));
                ui.label("Reading the scene manifest and the template volume…");
            });
        }
        if let Some(err) = self.last_error.clone() {
            callout(ui, pal.err, "✖  The scene could not be loaded", |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&err).monospace().small()).wrap());
                ui.horizontal(|ui| {
                    if ui
                        .button("Copy details")
                        .on_hover_text("Copy the full error message to the clipboard")
                        .clicked()
                    {
                        ctx.copy_text(err.clone());
                    }
                    if ui.button("Dismiss").clicked() {
                        self.last_error = None;
                    }
                });
            });
        }
        if self.embedded.is_none() || self.popped_out {
            ui.separator();
        }

        if self.popped_out && self.embedded.is_some() {
            let h = ui.available_height().max(unit * 6.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), h),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.add_space(unit * 2.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("The 3D Viewer is open in a separate window.")
                                .color(pal.text_muted),
                        )
                        .selectable(false),
                    );
                    ui.add_space(unit * 0.5);
                    if ui
                        .button("Dock to main window")
                        .on_hover_text(
                            "Close the separate window and show the 3D Viewer in the main window again",
                        )
                        .clicked()
                    {
                        self.popped_out = false;
                        ctx.request_repaint();
                    }
                },
            );
        } else if let Some(ref mut app) = self.embedded {
            let h = ui.available_height().max(unit * 8.0);
            ui.set_min_height(h);
            let host = vec![dbsloc_viz::HostButton {
                id: "open_viewer_popout",
                label: "Open Viewer in separate window".to_owned(),
                hover: "Show the 3D Viewer in a separate window that can be maximised or set to full screen (F11)"
                    .to_owned(),
                primary: true,
            }];
            if app.show_embedded_with(ui, frame, host) == Some("open_viewer_popout") {
                self.popped_out = true;
                ctx.request_repaint();
            }
        } else {
            let h = ui.available_height().max(unit * 6.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), h),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "No scene loaded.\n\n\
                                     Select a subject to fill in the default manifest path, \
                                     or browse for one; it loads by itself once that file exists.\n\
                                     A scene manifest is written by the Reconstruct step when \
                                     a template is configured.",
                                )
                                .color(pal.text_muted),
                            )
                            .wrap()
                            .selectable(false),
                        );
                    });
                },
            );
        }
        if let Some(app) = &self.embedded {
            self.navigation = app.navigation();
            state.navigation = self.navigation;
        }
        action
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subject_ids_match_with_or_without_the_prefix() {
        assert!(same_subject("sub-001", "001"));
        assert!(same_subject("001", "sub-001"));
        assert!(same_subject(" sub-001 ", "sub-001"));
        assert!(!same_subject("sub-001", "sub-002"));
    }

    #[test]
    fn a_subject_change_forgets_a_custom_backdrop_id() {
        let mut state = DbslocAppState {
            bids_dir: "a".to_owned(),
            ..Default::default()
        };
        let mut panel = ScenePanel::default();
        panel.sync_tracking(&mut state);
        let view = SceneViewState {
            backdrop_id: Some("custom:/x/y.nii".to_owned()),
            ..Default::default()
        };
        state.scene.view = Some(view);
        state.bids_dir = "b".to_owned();
        panel.sync_tracking(&mut state);
        assert_eq!(state.scene.view.unwrap().backdrop_id, None);
    }

    #[test]
    fn a_subject_change_keeps_a_catalogue_backdrop_id() {
        let mut state = DbslocAppState {
            bids_dir: "a".to_owned(),
            ..Default::default()
        };
        let mut panel = ScenePanel::default();
        panel.sync_tracking(&mut state);
        let view = SceneViewState {
            backdrop_id: Some("patient-preop".to_owned()),
            ..Default::default()
        };
        state.scene.view = Some(view);
        state.bids_dir = "b".to_owned();
        panel.sync_tracking(&mut state);
        assert_eq!(
            state.scene.view.unwrap().backdrop_id.as_deref(),
            Some("patient-preop")
        );
    }

    #[test]
    fn a_subject_change_keeps_the_atlas_id() {
        let mut state = DbslocAppState {
            bids_dir: "a".to_owned(),
            ..Default::default()
        };
        let mut panel = ScenePanel::default();
        panel.sync_tracking(&mut state);
        state.scene.view = Some(SceneViewState {
            atlas_id: Some("external:Morel".to_owned()),
            ..Default::default()
        });
        state.bids_dir = "b".to_owned();
        panel.sync_tracking(&mut state);
        assert_eq!(
            state.scene.view.unwrap().atlas_id.as_deref(),
            Some("external:Morel")
        );
    }

    #[test]
    fn threads_text_marks_all_cores() {
        assert_eq!(threads_text(16, 16), "16 (all)");
        assert_eq!(threads_text(4, 16), "4");
        assert_eq!(threads_text(1, 1), "1 (all)");
    }

    use super::*;
    use dbsloc_pipeline::reg_backend::RegBackend;
    use dbsloc_ui::{
        coregister_engine_blocker, normalize_engine_blocker, preprocess_engine_blocker,
    };

    fn full_readiness() -> PipelineReadiness {
        PipelineReadiness {
            has_bids_dir: true,
            has_subject: true,
            template_exists: true,
            has_raw_anat: true,
            has_raw_postop_ct: true,
            has_ants_registration: true,
            has_n4_bias_correction: true,
            ..Default::default()
        }
    }

    #[test]
    fn registration_engine_defaults_to_auto_and_old_state_still_loads() {
        let s = StageSettings::default();
        assert_eq!(s.normalize_engine, RegistrationEngine::Auto);
        assert_eq!(s.normalize_engine.value(), RegBackend::Auto);
        let old: StageSettings =
            serde_json::from_str(r#"{"run_vat": false}"#).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(old.preprocess_engine, RegistrationEngine::Auto);
        assert!(!old.run_vat);
    }

    #[test]
    fn registration_engine_survives_a_round_trip_and_maps_to_the_backend() {
        for (engine, backend) in [
            (RegistrationEngine::Auto, RegBackend::Auto),
            (RegistrationEngine::Gpu, RegBackend::Wgpu),
            (RegistrationEngine::Ants, RegBackend::Ants),
        ] {
            let s = StageSettings {
                coregister_engine: engine,
                ..Default::default()
            };
            let text = serde_json::to_string(&s).unwrap_or_default();
            let back: StageSettings = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(back.coregister_engine, engine);
            assert_eq!(back.coregister_engine.value(), backend);
        }
    }

    #[test]
    fn pacer_is_offered_only_in_a_build_that_contains_it() {
        let built = dbsloc_pipeline::PACER_AVAILABLE;
        assert_eq!(built, cfg!(feature = "pacer"));
        assert_eq!(recon_method_available(ReconMethod::Pacer), built);
        for v in [
            ReconMethod::Auto,
            ReconMethod::TracCore,
            ReconMethod::TracCoreNative,
        ] {
            assert!(recon_method_available(v));
            assert_eq!(recon_method_label(v), v.label());
            assert_eq!(recon_method_hint(v), v.hint());
        }
        if built {
            assert_eq!(recon_method_label(ReconMethod::Pacer), "PaCER");
        } else {
            assert_eq!(
                recon_method_label(ReconMethod::Pacer),
                "PaCER (not in this build)"
            );
            assert_eq!(recon_method_hint(ReconMethod::Pacer), PACER_NOT_IN_BUILD);
        }
        let mut saved = ReconMethod::Pacer;
        assert_eq!(drop_unavailable_recon_method(&mut saved), !built);
        let want = if built {
            ReconMethod::Pacer
        } else {
            ReconMethod::Auto
        };
        assert_eq!(saved, want);
        let mut other = ReconMethod::TracCore;
        assert!(!drop_unavailable_recon_method(&mut other));
        assert_eq!(other, ReconMethod::TracCore);
    }

    #[test]
    fn trac_core_native_maps_to_the_cli_and_survives_a_round_trip() {
        assert_eq!(ReconMethod::TracCoreNative.cli_value(), "trac_core_native");
        for method in [
            ReconMethod::Auto,
            ReconMethod::TracCore,
            ReconMethod::TracCoreNative,
            ReconMethod::Pacer,
        ] {
            let s = StageSettings {
                recon_method: method,
                ..Default::default()
            };
            let text = serde_json::to_string(&s).unwrap_or_default();
            let back: StageSettings = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(back.recon_method, method);
        }
    }

    #[test]
    fn norm_multispectral_defaults_on_and_t2_template_defaults_none() {
        let s = StageSettings::default();
        assert!(s.norm_multispectral);
        assert!(DbslocAppState::default()
            .normalization_template_t2
            .is_none());
    }

    #[test]
    fn build_request_carries_the_t2_template_and_multispectral_values() {
        let panel = StagePanel::default();
        let mut state = DbslocAppState {
            normalization_template_t2: Some("tpl_t2.nii.gz".to_owned()),
            ..Default::default()
        };
        state.stage.norm_multispectral = false;

        let req = panel
            .build_request(&state, &[Stage::Reconstruct])
            .unwrap_or_else(|| panic!("a non-empty stage list must build a request"));

        assert_eq!(
            req.normalization_template_t2.as_deref(),
            Some("tpl_t2.nii.gz")
        );
        assert!(!req.norm_multispectral);
        assert_eq!(req.recon_method, ReconMethod::Auto);
    }

    #[test]
    fn registration_method_label_reflects_the_engine() {
        for (engine, tag) in [
            (RegistrationEngine::Auto, "Auto"),
            (RegistrationEngine::Gpu, "GPU (wgpu)"),
            (RegistrationEngine::Ants, "ANTs (CPU)"),
        ] {
            let pre = registration_method_label(Stage::Preprocess, engine);
            assert!(pre.contains("N4 bias field correction"), "{pre}");
            assert!(pre.ends_with(tag), "{pre}");

            let coreg = registration_method_label(Stage::Coregister, engine);
            assert!(
                coreg.contains("Rigid registration (multi-start)"),
                "{coreg}"
            );
            assert!(coreg.ends_with(tag), "{coreg}");
        }
        assert!(registration_method_label(Stage::Normalize, RegistrationEngine::Gpu).is_empty());
    }

    #[test]
    fn normalize_engine_blocker_follows_the_engine() {
        let mut r = full_readiness();
        r.gpu_available = Some(false);
        assert!(normalize_engine_blocker(RegistrationEngine::Auto, &r).is_none());
        assert!(normalize_engine_blocker(RegistrationEngine::Ants, &r).is_none());
        assert!(normalize_engine_blocker(RegistrationEngine::Gpu, &r).is_some());
        r.has_ants_registration = false;
        assert!(normalize_engine_blocker(RegistrationEngine::Auto, &r).is_some());
        assert!(normalize_engine_blocker(RegistrationEngine::Ants, &r).is_some());
        r.gpu_available = Some(true);
        assert!(normalize_engine_blocker(RegistrationEngine::Auto, &r).is_none());
        assert!(normalize_engine_blocker(RegistrationEngine::Gpu, &r).is_none());
        r.gpu_available = None;
        assert!(normalize_engine_blocker(RegistrationEngine::Auto, &r).is_none());
    }

    #[test]
    fn normalize_is_not_blocked_by_missing_ants_when_the_gpu_can_run_it() {
        let mut r = full_readiness();
        r.has_ants_registration = false;
        r.gpu_available = Some(true);
        let s = StageSettings::default();
        let (state, _) = stage_status_detail(Stage::Normalize, &s, &r);
        assert_eq!(state, StageState::Ready);
        r.gpu_available = Some(false);
        let (state, why) = stage_status_detail(Stage::Normalize, &s, &r);
        assert_eq!(state, StageState::Blocked);
        assert!(why.contains("graphics card"), "{why}");
    }

    #[test]
    fn preprocess_and_coregister_follow_the_engine_like_normalize() {
        let mut r = full_readiness();
        r.has_ants_registration = false;
        r.has_n4_bias_correction = false;
        let s = StageSettings::default();

        r.gpu_available = Some(true);
        assert!(preprocess_engine_blocker(RegistrationEngine::Auto, &r).is_none());
        assert!(coregister_engine_blocker(RegistrationEngine::Auto, &r).is_none());
        assert_eq!(
            stage_status_detail(Stage::Preprocess, &s, &r).0,
            StageState::Ready
        );
        assert_eq!(
            stage_status_detail(Stage::Coregister, &s, &r).0,
            StageState::Ready
        );

        r.gpu_available = Some(false);
        assert!(preprocess_engine_blocker(RegistrationEngine::Auto, &r).is_some());
        assert!(coregister_engine_blocker(RegistrationEngine::Auto, &r).is_some());
        assert_eq!(
            stage_status_detail(Stage::Preprocess, &s, &r).0,
            StageState::Blocked
        );
        assert_eq!(
            stage_status_detail(Stage::Coregister, &s, &r).0,
            StageState::Blocked
        );
    }

    #[test]
    fn ants_banner_is_silent_for_a_gpu_run() {
        assert!(!ants_programs_needed(RegistrationEngine::Gpu, Some(false)));
        assert!(!ants_programs_needed(RegistrationEngine::Auto, Some(true)));
        assert!(!ants_programs_needed(RegistrationEngine::Auto, None));
        assert!(ants_programs_needed(RegistrationEngine::Auto, Some(false)));
        assert!(ants_programs_needed(RegistrationEngine::Ants, Some(true)));
    }

    #[test]
    fn template_grid_cache_is_empty_for_an_unreadable_template() {
        let mut cache = TemplateGridCache::default();
        assert_eq!(cache.voxel_mm(Some("/nonexistent/template.nii.gz")), None);
        assert_eq!(cache.grid_dims(Some(1.0)), None);
        assert_eq!(cache.grid_dims(None), None);
    }

    #[test]
    fn norm_resolution_defaults_to_the_template_resolution() {
        let s = StageSettings::default();
        assert_eq!(s.norm_resolution, NormResolution::Template);
        assert_eq!(s.norm_resolution.value(), NormGrid::Template);
        assert_eq!(NormResolution::FullTemplate.value(), NormGrid::Full);
        assert_eq!(NormResolution::Mm1p0.value(), NormGrid::Mm(1.0));
    }

    #[test]
    fn persisted_implicit_defaults_read_as_the_new_default() {
        for old in ["Auto", "Mm1p0"] {
            let s: StageSettings = serde_json::from_str(&format!(
                r#"{{"norm_resolution": "{old}", "run_vat": false}}"#
            ))
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(s.norm_resolution, NormResolution::Template, "{old}");
            assert!(!s.run_vat);
        }
    }

    #[test]
    fn the_grid_cache_resolves_the_default_without_a_template() {
        let mut cache = TemplateGridCache::default();
        assert_eq!(cache.voxel_mm(Some("/nonexistent/template.nii.gz")), None);
        let r = cache.resolved(NormGrid::Template, RegBackend::Ants, 1);
        assert_eq!(r.mm, Some(1.0));
        assert!(r.reason.is_none());
        let r = cache.resolved(NormGrid::Mm(0.8), RegBackend::Ants, 2);
        assert_eq!(r.mm, Some(0.8));
        assert_eq!(stepped_grid_text(0.5, 0.8), "Registration grid stepped to 0.8 mm: the graphics card cannot fit the template's 0.5 mm grid.");
    }

    #[test]
    fn full_template_resolution_survives_a_round_trip() {
        let s = StageSettings {
            norm_resolution: NormResolution::FullTemplate,
            ..Default::default()
        };
        let text = serde_json::to_string(&s).unwrap_or_default();
        let back: StageSettings = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.norm_resolution, NormResolution::FullTemplate);
    }

    #[test]
    fn unreachable_reports_downstream_when_normalize_is_off() {
        let settings = StageSettings {
            run_norm: false,
            ..Default::default()
        };
        let unreachable = settings.unreachable_stages(&PipelineReadiness {
            has_raw_anat: true,
            has_raw_postop_ct: true,
            ..Default::default()
        });

        assert!(unreachable.contains(&Stage::Reconstruct));
        assert!(unreachable.contains(&Stage::Orientation));
        assert!(unreachable.contains(&Stage::Vat));
        assert!(unreachable.contains(&Stage::Visualize));
    }

    #[test]
    fn unreachable_is_empty_when_outputs_already_exist() {
        let settings = StageSettings {
            run_norm: false,
            ..Default::default()
        };
        let readiness = PipelineReadiness {
            has_raw_anat: true,
            has_coreg_warped: true,
            has_norm_nifti: true,
            has_norm_postop_ct: true,
            has_reconstruction_json: true,
            has_raw_postop_ct: true,
            ..Default::default()
        };

        assert!(settings.unreachable_stages(&readiness).is_empty());
    }

    #[test]
    fn unreachable_never_mutates_the_selection() {
        let mut settings = StageSettings {
            run_norm: false,
            ..Default::default()
        };
        let readiness = PipelineReadiness {
            has_raw_anat: true,
            has_raw_postop_ct: true,
            template_exists: true,
            ..Default::default()
        };
        let _ = settings.unreachable_stages(&readiness);
        assert!(settings.run_reconstruct, "querying must not untick a step");

        settings.deselect_unreachable(&readiness);
        assert!(!settings.run_reconstruct, "the explicit action does untick");
        assert!(settings.run_preprocess, "and only touches what it reported");
        assert!(settings.run_coreg, "a reachable step keeps its tick");
    }

    #[test]
    fn selected_runnable_stages_skip_existing_outputs_without_overwrite() {
        let settings = StageSettings::default();
        let readiness = PipelineReadiness {
            has_preprocess_nifti: true,
            has_norm_nifti: true,
            has_reconstruction_json: true,
            ..Default::default()
        };
        let stages = settings.selected_runnable_stages(&readiness);

        assert!(!stages.contains(&Stage::Preprocess));
        assert!(!stages.contains(&Stage::Normalize));
        assert!(!stages.contains(&Stage::Reconstruct));
        assert!(stages.contains(&Stage::Coregister));
    }

    #[test]
    fn overwrite_lists_only_stages_with_existing_output() {
        let settings = StageSettings {
            overwrite_results: true,
            ..Default::default()
        };
        let readiness = PipelineReadiness {
            has_preprocess_nifti: true,
            has_reconstruction_json: true,
            ..Default::default()
        };

        assert_eq!(
            settings.stages_that_would_be_overwritten(&readiness),
            vec![Stage::Preprocess, Stage::Reconstruct]
        );
    }

    #[test]
    fn missing_electrode_model_blocks_reconstruction() {
        let settings = StageSettings::default();
        let readiness = full_readiness();
        let runnable = settings.selected_runnable_stages(&readiness);
        let blockers = settings.selection_blockers(&readiness, &runnable);

        assert!(blockers.iter().any(|b| b.contains("electrode model")));
    }

    #[test]
    fn ct_detection_uses_the_bids_suffix_only() {
        assert!(is_ct_nifti_name("sub-01_ses-postop_ct.nii.gz"));
        assert!(is_ct_nifti_name("sub-01_acq-postop_ct.nii.gz"));
        assert!(is_ct_nifti_name(
            "sub-01_ses-postop_space-MNI152_desc-preproc_CT.nii.gz"
        ));
        assert!(!is_ct_nifti_name("sub-01_acq-postop_T1w.nii.gz"));
        assert!(!is_ct_nifti_name("sub-01_ses-postop_T2w.nii.gz"));
        assert!(!is_ct_nifti_name("sub-01_desc-corrected_T1w.nii.gz"));
        assert!(!is_ct_nifti_name("sub-01_desc-reconstruct_T1w.nii"));
        assert!(!is_ct_nifti_name("sub-01_ct.png"));
    }

    #[test]
    fn humanize_strips_machine_artefacts_from_user_text() {
        assert_eq!(
            humanize_message(r"no anchor under \\?\C:\data\sub-01"),
            r"no anchor under C:\data\sub-01"
        );
        assert_eq!(
            humanize_message(r"under \\?\UNC\server\share"),
            r"under \\server\share"
        );
        assert_eq!(
            humanize_message("BIDS error: BIDS error: no pre-op anchor"),
            "BIDS error: no pre-op anchor"
        );
        assert_eq!(
            humanize_message("BIDS error: subject resolution failed: BIDS error: no pre-op anchor"),
            "BIDS error: subject resolution failed: no pre-op anchor"
        );
        assert_eq!(humanize_message("plain message"), "plain message");
        assert_eq!(
            humanize_message("stage: coregister: failed"),
            "stage: coregister: failed"
        );
    }

    #[test]
    fn the_summary_after_a_rejection_reports_rework_not_a_failure() {
        let (tx, rx) = mpsc::channel();
        let mut log = LogPanel {
            event_rx: Some(rx),
            ..Default::default()
        };
        let mut stages = StagePanel::default();
        tx.send(RunEvent::StageAwaitingQc {
            stage: "Coregister".to_owned(),
        })
        .unwrap();
        tx.send(RunEvent::StageFailed {
            stage: "Coregister".to_owned(),
            message: "rejected".to_owned(),
        })
        .unwrap();
        tx.send(RunEvent::PipelineCompleted {
            stages_ok: 0,
            stages_failed: 1,
            stages_skipped: 0,
        })
        .unwrap();
        log.poll_events(&mut stages);
        let last = log.lines.back().expect("summary line");
        assert_eq!(last.raw, "Pipeline stopped: 1 needs rework");
        assert_eq!(log.rework, 0);
    }

    #[test]
    fn a_deferred_gate_in_a_batch_is_prefixed_and_reads_as_awaiting_review() {
        let (tx, rx) = mpsc::channel();
        let mut log = LogPanel {
            event_rx: Some(rx),
            prefix: Some("sub-02".to_owned()),
            defer_review: true,
            ..Default::default()
        };
        let mut stages = StagePanel::default();
        for ev in [
            RunEvent::StageAwaitingQc {
                stage: "coregister".to_owned(),
            },
            RunEvent::StageFailed {
                stage: "coregister".to_owned(),
                message: "coreg: qc not approved".to_owned(),
            },
            RunEvent::PipelineCompleted {
                stages_ok: 1,
                stages_failed: 1,
                stages_skipped: 0,
            },
        ] {
            tx.send(ev).unwrap();
        }
        log.poll_events(&mut stages);
        let raws: Vec<&str> = log.lines.iter().map(|l| l.raw.as_str()).collect();
        assert_eq!(raws.len(), 2, "the paired failure is not shown: {raws:?}");
        assert!(raws[0].starts_with("[sub-02] ◔ coregister: registration awaits review"));
        assert_eq!(
            raws[1],
            "[sub-02] Pipeline paused: 1 completed, 0 failed, 0 skipped, 1 awaiting review"
        );
        assert!(!log.lines.iter().any(|l| l.kind == LogKind::Failed));
    }

    #[test]
    fn a_batch_needs_two_ticked_subjects() {
        let mut info = BatchInfo::default();
        assert!(!info.is_batch());
        info.subjects = vec!["sub-01".to_owned()];
        assert!(!info.is_batch());
        info.subjects.push("sub-02".to_owned());
        assert!(info.is_batch());
    }

    #[test]
    fn a_runner_that_disconnects_without_finishing_is_reported() {
        let mut stages = StagePanel {
            running: true,
            ..Default::default()
        };
        let mut log = LogPanel::default();
        let (tx, rx) = mpsc::channel();
        log.attach(rx);
        tx.send(RunEvent::StageStarted {
            stage: "coregister".to_owned(),
        })
        .unwrap();
        drop(tx);
        let events = log.poll_events(&mut stages);
        assert_eq!(events.len(), 1);
        assert!(!stages.running);
        assert!(log.take_unexpected_end());
        assert!(!log.take_unexpected_end());

        let (tx, rx) = mpsc::channel();
        log.attach(rx);
        tx.send(RunEvent::PipelineCompleted {
            stages_ok: 1,
            stages_failed: 0,
            stages_skipped: 0,
        })
        .unwrap();
        drop(tx);
        log.poll_events(&mut stages);
        assert!(!log.take_unexpected_end());
    }

    #[test]
    fn the_engine_info_text_carries_no_run_of_spaces() {
        for step in ["Preprocess", "Coregister", "Normalize"] {
            let text = engine_info_text(step);
            assert!(!text.contains("  "), "{text}");
        }
        assert!(!STOP_HOVER.contains("  "), "{STOP_HOVER}");
    }

    #[test]
    fn the_subject_folder_is_joined_from_components() {
        let mut state = DbslocAppState {
            bids_dir: "root".to_owned(),
            ..Default::default()
        };
        let parts = |state: &DbslocAppState| -> Vec<_> {
            state
                .subject_derivatives_dir("sub-01")
                .unwrap_or_default()
                .components()
                .map(|c| c.as_os_str().to_owned())
                .collect()
        };
        assert_eq!(parts(&state), ["root", "derivatives", "leaddbs", "sub-01"]);
        state.output_dirs.push(crate::app::DatasetOutputDir {
            bids_dir: "root".to_owned(),
            output_dir: "out".to_owned(),
        });
        assert_eq!(parts(&state), ["out", "sub-01"]);
        state.bids_dir = "other".to_owned();
        assert_eq!(parts(&state), ["other", "derivatives", "leaddbs", "sub-01"]);
    }

    #[test]
    fn the_readiness_of_another_subject_is_never_shown() {
        let mut cache = ReadinessCache {
            current: full_readiness(),
            current_for: Some(("D:/a".to_owned(), "sub-01".to_owned(), None)),
            ..Default::default()
        };
        let mut state = DbslocAppState {
            bids_dir: "D:/a".to_owned(),
            selected_subject: Some("sub-01".to_owned()),
            ..Default::default()
        };
        cache.forget_other_subject(&state);
        assert!(cache.get().has_subject);
        state.selected_subject = Some("sub-02".to_owned());
        cache.forget_other_subject(&state);
        assert_eq!(cache.get(), &PipelineReadiness::default());

        cache.current = full_readiness();
        cache.current_for = Some(("D:/a".to_owned(), "sub-02".to_owned(), None));
        state.output_dirs.push(crate::app::DatasetOutputDir {
            bids_dir: "D:/a".to_owned(),
            output_dir: "D:/out".to_owned(),
        });
        cache.forget_other_subject(&state);
        assert_eq!(cache.get(), &PipelineReadiness::default());
    }

    #[test]
    fn readiness_counts_as_current_again_only_after_the_rescan_that_follows_an_invalidation() {
        let mut cache = ReadinessCache::default();
        let state = DbslocAppState {
            bids_dir: std::env::temp_dir()
                .join(format!("dbsloc_ready_{}", std::process::id()))
                .to_string_lossy()
                .into_owned(),
            selected_subject: Some("sub-01".to_owned()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let settle = |cache: &mut ReadinessCache| {
            for _ in 0..1000 {
                cache.poll(&ctx, &state);
                if cache.is_current_for(&state) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the readiness scan did not finish");
        };
        settle(&mut cache);
        cache.invalidate();
        assert!(!cache.is_current_for(&state));
        settle(&mut cache);
        assert!(cache.is_current_for(&state));

        cache.invalidate();
        let crashed = |cache: &mut ReadinessCache| {
            let (tx, rx) = mpsc::channel::<ReadinessMsg>();
            drop(tx);
            cache.pending = Some(rx);
            cache.key = Some((subject_key(&state), String::new()));
        };
        crashed(&mut cache);
        cache.poll(&ctx, &state);
        assert!(cache.pending.is_some(), "a crashed scan is retried once");
        settle(&mut cache);
        crashed(&mut cache);
        cache.poll(&ctx, &state);
        assert!(
            cache.pending.is_none(),
            "no second retry in the same generation"
        );
    }

    #[test]
    fn log_panel_trims_and_keeps_filter_indices_valid() {
        let mut log = LogPanel::default();
        for i in 0..(LOG_CAPACITY + LOG_TRIM_BLOCK) {
            log.push(format!("line {i}"));
        }
        assert!(log.lines.len() <= LOG_CAPACITY);
        assert!(log.dropped > 0);
        for idx in &log.visible {
            assert!(*idx < log.lines.len());
        }
    }

    #[test]
    fn detail_is_a_view_filter_and_never_drops_stored_lines() {
        let mut log = LogPanel::default();
        log.push("[out] chatty tool line");
        log.push("[err] a warning");
        log.push("internal note");

        log.detail = OutputDetail::Standard;
        log.rebuild_visible();
        assert_eq!(log.visible.len(), 2, "stdout is hidden at Standard");

        log.detail = OutputDetail::Detailed;
        log.rebuild_visible();
        assert_eq!(log.visible.len(), 3, "raising the detail brings it back");
        assert_eq!(log.lines.len(), 3, "nothing was discarded on ingest");

        assert_eq!(
            log.lines[0].display(OutputDetail::Debug),
            "[out] chatty tool line"
        );
        assert_eq!(
            log.lines[0].display(OutputDetail::Detailed),
            "chatty tool line"
        );
    }

    #[test]
    fn log_panel_filter_selects_matching_lines_only() {
        let mut log = LogPanel {
            detail: OutputDetail::Detailed,
            ..Default::default()
        };
        log.push("alpha");
        log.push("beta");
        log.push_error("gamma failed");
        log.filter = "a".to_owned();
        log.rebuild_visible();
        assert_eq!(log.visible.len(), 3);

        log.filter = "beta".to_owned();
        log.rebuild_visible();
        assert_eq!(log.visible.len(), 1);

        log.filter.clear();
        log.problems_only = true;
        log.rebuild_visible();
        assert_eq!(log.visible.len(), 1);
    }

    fn review_scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_panels_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let anat = dir.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x")
            .unwrap_or_else(|e| panic!("{e}"));
        dir
    }

    fn write_log(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap_or_else(|| panic!("no parent")))
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(path, body).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn readiness_lists_the_subject_images_for_the_view_buttons() {
        let root = review_scratch("images");
        let preop = root.join("rawdata/sub-1/ses-preop/anat");
        let postop = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&postop).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(preop.join("sub-1_ses-preop_T2w.nii.gz"), b"x")
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(postop.join("sub-1_ses-postop_CT.nii.gz"), b"x")
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(postop.join("sub-1_ses-postop_acq-ax_T1w.nii.gz"), b"x")
            .unwrap_or_else(|e| panic!("{e}"));
        let ready = scan_readiness(&root.to_string_lossy(), None, "1", None);
        let labels: Vec<&str> = ready.images.iter().map(|r| r.label.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.starts_with("Pre-op T1w")),
            "{labels:?}"
        );
        assert!(labels.contains(&"Pre-op T2w"), "{labels:?}");
        assert!(labels.contains(&"Post-op CT"), "{labels:?}");
        assert!(labels.contains(&"Post-op MRI (axial)"), "{labels:?}");
        assert!(ready.images.iter().all(|r| r.path.is_file()));
        assert!(ready.anchor_image().is_some());
        let ct = ready.postop_images_for(dbsloc_ui::PostopModality::Ct);
        assert_eq!(ct.len(), 1);
        assert!(ct[0].is_ct);
        let mri = ready.postop_images_for(dbsloc_ui::PostopModality::Mri);
        assert_eq!(mri.len(), 1);
        assert_eq!(mri[0].kind, ImageKind::PostopMr);
        let auto = ready.postop_images_for(dbsloc_ui::PostopModality::Auto);
        assert_eq!(auto.len(), 1);
        assert_eq!(auto[0].kind, ImageKind::PostopCt);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_view_request_opens_the_one_preview_and_a_second_replaces_it() {
        let ctx = egui::Context::default();
        let mut host = ImagePreviewHost::default();
        let row = |name: &str| ImageRow {
            kind: ImageKind::PreopAnchor,
            label: name.to_owned(),
            path: PathBuf::from(format!("missing/{name}.nii.gz")),
            is_ct: false,
        };
        let frame = |host: &mut ImagePreviewHost| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(input, |ui| host.show(ui.ctx()))
                .textures_delta
                .clear();
        };
        frame(&mut host);
        assert!(host.current_file().is_none());
        request_image_preview(&ctx, &row("a"));
        frame(&mut host);
        assert_eq!(host.current_file(), Some("a.nii.gz"));
        request_image_preview(&ctx, &row("b"));
        frame(&mut host);
        assert_eq!(host.current_file(), Some("b.nii.gz"));
    }

    #[test]
    fn readiness_reads_the_review_state_from_the_method_logs() {
        let root = review_scratch("reviews");
        let sub = dbsloc_pipeline::bids::resolve_subject_with(&root, "1", false)
            .unwrap_or_else(|e| panic!("{e}"));
        let coreg = dbsloc_pipeline::derivatives::coreg_method_log(&sub);
        let norm = dbsloc_pipeline::derivatives::norm_method_log(&sub);
        assert_eq!(
            registration_reviews(&sub),
            (
                RegistrationReview::NotReviewed,
                RegistrationReview::NotReviewed
            ),
            "missing logs"
        );
        write_log(&coreg, r#"{"approval": {"CT": 1}}"#);
        write_log(&norm, r#"{"approval": 0}"#);
        assert_eq!(
            registration_reviews(&sub),
            (
                RegistrationReview::Approved,
                RegistrationReview::NotReviewed
            ),
            "a stage-written zero is not a rejection"
        );
        write_log(&norm, r#"{"approval": 0, "reviewed": true}"#);
        assert_eq!(
            registration_reviews(&sub),
            (RegistrationReview::Approved, RegistrationReview::Rejected)
        );
        write_log(
            &coreg,
            r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": true}"#,
        );
        write_log(&norm, r#"{"approval": 1}"#);
        assert_eq!(
            registration_reviews(&sub),
            (RegistrationReview::Rejected, RegistrationReview::Approved)
        );
        write_log(&coreg, r#"{"approval": null}"#);
        write_log(&norm, "not json");
        assert_eq!(
            registration_reviews(&sub),
            (
                RegistrationReview::NotReviewed,
                RegistrationReview::NotReviewed
            ),
            "undecided and unreadable logs"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_registration_card_shows_the_review_state_only_when_it_has_output() {
        let settings = StageSettings::default();
        let mut r = PipelineReadiness {
            coreg_review: RegistrationReview::Approved,
            ..full_readiness()
        };
        let card = card_review(
            &r,
            &settings,
            Stage::Coregister,
            false,
            None,
            RefineActivity::Idle,
        );
        assert_eq!(card.chip, None);
        assert!(!card.can_review);
        r.has_coreg_warped = true;
        let card = card_review(
            &r,
            &settings,
            Stage::Coregister,
            false,
            None,
            RefineActivity::Idle,
        );
        assert_eq!(card.chip, Some(RegistrationReview::Approved));
        assert!(card.can_review);
        let card = card_review(
            &r,
            &settings,
            Stage::Coregister,
            true,
            None,
            RefineActivity::Idle,
        );
        assert_eq!(card.chip, Some(RegistrationReview::Approved));
        assert!(!card.can_review, "no review while a run is active");
        let card = card_review(
            &r,
            &settings,
            Stage::Preprocess,
            false,
            None,
            RefineActivity::Idle,
        );
        assert_eq!(card.chip, None);
        assert!(!card.can_review);
    }

    #[test]
    fn a_dependent_card_carries_the_upstream_warning() {
        let settings = StageSettings::default().only(Stage::Reconstruct);
        let r = PipelineReadiness {
            has_norm_nifti: true,
            norm_review: RegistrationReview::Rejected,
            ..full_readiness()
        };
        let card = card_review(
            &r,
            &settings,
            Stage::Reconstruct,
            false,
            None,
            RefineActivity::Idle,
        );
        assert_eq!(card.warnings.len(), 1);
        assert_eq!(card.warnings[0].upstream, Stage::Normalize);
        assert_eq!(
            card.warnings[0].text(),
            "Uses a normalization that was rejected"
        );
    }

    #[test]
    fn a_short_path_is_kept_and_a_long_one_keeps_its_end() {
        let short = Path::new("/data/sub-01");
        assert_eq!(shorten_path(short, 40), short.display().to_string());
        let long = Path::new("/Users/someone/code/derivatives/group/exports/tables");
        let text = shorten_path(long, 24);
        assert!(text.starts_with('…'));
        assert!(text.ends_with("exports/tables"));
        assert!(text.chars().count() <= 24);
        assert!(!text.contains("Users"));
    }

    #[test]
    fn only_steps_with_a_folder_get_the_button_and_the_folders_are_distinct_from_inputs() {
        assert_eq!(stage_output_folder(Stage::Normalize), Some("normalization"));
        assert_eq!(
            stage_output_folder(Stage::Orientation),
            Some("reconstruction")
        );
        assert_eq!(stage_output_folder(Stage::Bids), None);
        assert_eq!(stage_output_folder(Stage::Visualize), None);
    }

    #[test]
    fn card_actions_map_to_panel_actions() {
        assert!(matches!(
            card_to_panel_action(
                CardAction::ReviewStage(Stage::Normalize),
                Stage::Reconstruct
            ),
            StagePanelAction::ReviewStage(Stage::Normalize)
        ));
        assert!(matches!(
            card_to_panel_action(CardAction::RerunStep, Stage::Coregister),
            StagePanelAction::RerunStep(Stage::Coregister)
        ));
        assert!(matches!(
            card_to_panel_action(CardAction::ReviewNow, Stage::Coregister),
            StagePanelAction::ReviewNow
        ));
        assert!(matches!(
            card_to_panel_action(CardAction::OpenFolder, Stage::Normalize),
            StagePanelAction::OpenStageFolder(Stage::Normalize)
        ));
        assert!(matches!(
            card_to_panel_action(CardAction::None, Stage::Coregister),
            StagePanelAction::None
        ));
        assert!(matches!(
            card_to_panel_action(CardAction::RefineLocalization, Stage::Reconstruct),
            StagePanelAction::RefineLocalization
        ));
    }

    #[test]
    fn the_refine_button_belongs_to_a_reconstruction_that_exists() {
        let settings = StageSettings::default();
        let mut r = PipelineReadiness {
            has_reconstruction_json: false,
            ..full_readiness()
        };
        let idle = RefineActivity::Idle;
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert!(!card.refine.shown);
        r.has_reconstruction_json = true;
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert!(card.refine.shown && card.refine.enabled && !card.refine.corrected);
        let card = card_review(&r, &settings, Stage::Normalize, false, None, idle);
        assert!(!card.refine.shown);
        let card = card_review(&r, &settings, Stage::Reconstruct, true, None, idle);
        assert!(card.refine.shown && !card.refine.enabled);
        let card = card_review(
            &r,
            &settings,
            Stage::Reconstruct,
            false,
            None,
            RefineActivity::Saving,
        );
        assert!(card.refine.shown && !card.refine.enabled);
    }

    #[test]
    fn the_rerun_warning_needs_a_correction_and_a_selected_rerun() {
        let mut settings = StageSettings::default();
        let r = PipelineReadiness {
            has_reconstruction_json: true,
            reco_manually_corrected: true,
            ..full_readiness()
        };
        let idle = RefineActivity::Idle;
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert!(card.refine.corrected && !card.refine.rerun_replaces);
        settings.set_selected(Stage::Reconstruct, true);
        settings.overwrite_results = true;
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert!(card.refine.rerun_replaces);
        settings.set_selected(Stage::Reconstruct, false);
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert!(!card.refine.rerun_replaces);
    }

    #[test]
    fn the_corrected_flag_is_read_from_the_reconstruction_file() {
        let reco_manually_corrected =
            |p: &Path| read_json(p).is_some_and(|v| manually_corrected_in(&v));
        let dir = review_scratch("reco_flag");
        let path = dir.join("reco.json");
        assert!(!reco_manually_corrected(&path));
        std::fs::write(&path, r#"{"props": {"1": {"manually_corrected": false}}}"#).unwrap();
        assert!(!reco_manually_corrected(&path));
        std::fs::write(
            &path,
            r#"{"props": {"1": {"manually_corrected": false}, "2": {"manually_corrected": true}}}"#,
        )
        .unwrap();
        assert!(reco_manually_corrected(&path));
        std::fs::write(&path, "not json").unwrap();
        assert!(!reco_manually_corrected(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reco_sides_report_review_failure_and_flags_per_lead() {
        let v = serde_json::json!({
            "native": {"1": {"markers": {}}},
            "mni": {"1": {"markers": {}}},
            "props": {"1": {"manually_corrected": false}},
            "meta": {
                "manual_correction": {"1": {"review": "accepted", "roll_review": null}},
                "reconstruct": {
                    "status": {"1": "traced", "2": "failed"},
                    "failed": {"2": "left: TRAC/CORE: no seed"},
                    "flags": {"1": ["vertical_marker"]}
                }
            }
        });
        let sides = reco_sides_in(&v);
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[0].label, "Right lead");
        assert_eq!(sides[0].review, LeadReview::Accepted);
        assert!(sides[0].text().contains("vertical segment marker"));
        assert_eq!(sides[1].review, LeadReview::NotFound);
        assert!(sides[1].hover().contains("no seed"));
        let corrected = serde_json::json!({
            "mni": {"1": {}, "2": {}},
            "props": {"2": {"manually_corrected": true}}
        });
        let sides = reco_sides_in(&corrected);
        assert_eq!(sides[0].review, LeadReview::Unchecked);
        assert_eq!(sides[1].review, LeadReview::Corrected);
        assert_eq!(sides[0].text(), "Right lead: unchecked");
    }

    #[test]
    fn the_card_shows_the_scanned_lead_states_and_warns_on_an_accepted_lead() {
        let mut settings = StageSettings::default();
        settings.set_selected(Stage::Reconstruct, true);
        settings.overwrite_results = true;
        let r = PipelineReadiness {
            has_reconstruction_json: true,
            reco_sides: vec![RecoSideSummary {
                label: "Left lead",
                review: LeadReview::Accepted,
                reason: None,
                flags: Vec::new(),
            }],
            ..full_readiness()
        };
        let idle = RefineActivity::Idle;
        let card = card_review(&r, &settings, Stage::Reconstruct, false, None, idle);
        assert_eq!(card.reco_sides.len(), 1);
        assert!(card.refine.rerun_replaces && !card.refine.corrected);
        let card = card_review(&r, &settings, Stage::Normalize, false, None, idle);
        assert!(card.reco_sides.is_empty());
    }

    #[test]
    fn orientation_counts_only_once_the_stage_wrote_its_report() {
        assert!(!orientation_in(&serde_json::json!({})));
        assert!(!orientation_in(
            &serde_json::json!({"meta": {"orientation": "unresolved"}})
        ));
        assert!(orientation_in(
            &serde_json::json!({"meta": {"orientation": {"1": {"status": "resolved"}}}})
        ));
    }

    fn scene_state(sub: &str, manifest: &str, manual: bool) -> DbslocAppState {
        let mut state = DbslocAppState {
            bids_dir: "bids_root".to_owned(),
            selected_subject: Some(sub.to_owned()),
            ..Default::default()
        };
        state.scene.manifest_path = manifest.to_owned();
        state.scene.manifest_path_is_manual = manual;
        state
    }

    #[test]
    fn stale_manual_manifest_falls_back_to_default_on_first_sync() {
        let mut state = scene_state("sub-01", "does_not_exist_manifest.json", true);
        ScenePanel::default().sync_tracking(&mut state);
        assert!(!state.scene.manifest_path_is_manual);
        assert_eq!(
            state.scene.manifest_path,
            default_manifest_path(&scene_state("sub-01", "", false), "sub-01")
        );
    }

    #[test]
    fn existing_manual_manifest_is_kept_on_first_sync() {
        let file = std::env::temp_dir().join(format!(
            "dbsloc_manual_manifest_{}.json",
            std::process::id()
        ));
        std::fs::write(&file, "{}").unwrap();
        let path = file.to_string_lossy().into_owned();
        let mut state = scene_state("sub-01", &path, true);
        ScenePanel::default().sync_tracking(&mut state);
        std::fs::remove_file(&file).unwrap();
        assert!(state.scene.manifest_path_is_manual);
        assert_eq!(state.scene.manifest_path, path);
    }

    #[test]
    fn subject_change_clears_manual_manifest() {
        let file = std::env::temp_dir().join(format!(
            "dbsloc_manual_manifest_change_{}.json",
            std::process::id()
        ));
        std::fs::write(&file, "{}").unwrap();
        let path = file.to_string_lossy().into_owned();
        let mut state = scene_state("sub-01", &path, true);
        let mut panel = ScenePanel::default();
        panel.sync_tracking(&mut state);
        assert!(state.scene.manifest_path_is_manual);
        state.selected_subject = Some("sub-02".to_owned());
        panel.sync_tracking(&mut state);
        std::fs::remove_file(&file).unwrap();
        assert!(!state.scene.manifest_path_is_manual);
        assert_eq!(
            state.scene.manifest_path,
            default_manifest_path(&scene_state("sub-02", "", false), "sub-02")
        );
    }

    #[test]
    fn an_output_folder_change_moves_the_default_manifest() {
        let mut state = scene_state("sub-01", "", false);
        let mut panel = ScenePanel::default();
        panel.sync_tracking(&mut state);
        let before = state.scene.manifest_path.clone();
        assert!(Path::new(&before).starts_with(Path::new("bids_root").join("derivatives")));
        let out = std::env::temp_dir().join("dbsloc_panels_out");
        state.output_dirs.push(crate::app::DatasetOutputDir {
            bids_dir: "bids_root".to_owned(),
            output_dir: out.to_string_lossy().into_owned(),
        });
        panel.sync_tracking(&mut state);
        assert!(Path::new(&state.scene.manifest_path).starts_with(out.join("sub-01")));
    }

    #[test]
    fn load_default_manifest_resets_manual_choice() {
        let mut state = scene_state("sub-01", "stale.json", true);
        let ctx = egui::Context::default();
        let mut panel = ScenePanel::default();
        let res = panel.load_default_manifest(&mut state, &ctx, true);
        assert!(res.is_err());
        assert!(!state.scene.manifest_path_is_manual);
        assert_eq!(
            state.scene.manifest_path,
            default_manifest_path(&scene_state("sub-01", "", false), "sub-01")
        );
    }
}
