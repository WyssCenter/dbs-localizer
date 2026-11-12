// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The pipeline step-selection state machine and the static step-card copy.
//!
//! Pure data and pure logic: no `eframe`, no `egui`, no filesystem, no threads.
//! `dbsloc-app` renders these settings in egui widgets; `dbsloc-web` will do the
//! same in its own screens. The mapping to the pipeline's `RunOptions` is here
//! ([`StageSettings::run_request`]); the filesystem scan that fills
//! [`crate::PipelineReadiness`] stays in the front end that can do I/O.

use std::path::PathBuf;

use dbsloc_protocol::reg_backend::RegBackend;
use dbsloc_protocol::stage::Stage;
use dbsloc_protocol::{NormGrid, DEFAULT_NORM_ISOTROPIC_MM};
use serde::{Deserialize, Serialize};

use crate::batch::ReviewMode;
use crate::readiness::{PipelineReadiness, RegistrationReview, StageState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconMethod {
    Auto,
    TracCore,
    TracCoreNative,
    Pacer,
}

impl ReconMethod {
    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::TracCore => "trac_core",
            Self::TracCoreNative => "trac_core_native",
            Self::Pacer => "pacer",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::TracCore => "TRAC / CORE",
            Self::TracCoreNative => "TRAC / CORE (native CT)",
            Self::Pacer => "PaCER",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Auto => "Let the pipeline pick the reconstruction method for this image.",
            Self::TracCore => "Trajectory search followed by contact refinement (TRAC / CORE).",
            Self::TracCoreNative => {
                "Trace the coregistered CT in its own space. On a CT this behaves like TRAC / CORE; \
                 it is rejected for a post-operative MRI."
            }
            Self::Pacer => "Precise and Convenient Electrode Reconstruction (PaCER).",
        }
    }
}

/// The *Resolution* picker. Settings saved before the template-resolution default
/// existed stored `Mm1p0` (or the older `Auto`) for everybody who never touched the picker, so
/// those spellings load as [`NormResolution::Template`]; an explicit 1.0 mm choice made since
/// is saved under its own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NormResolution {
    #[default]
    #[serde(alias = "Auto", alias = "Mm1p0")]
    Template,
    #[serde(rename = "Mm1p0Chosen")]
    Mm1p0,
    Mm0p8,
    Mm0p5,
    FullTemplate,
    Custom,
}

impl NormResolution {
    pub fn value(self) -> NormGrid {
        match self {
            Self::Template => NormGrid::Template,
            Self::Mm1p0 => NormGrid::Mm(DEFAULT_NORM_ISOTROPIC_MM),
            Self::Mm0p8 => NormGrid::Mm(0.8),
            Self::Mm0p5 => NormGrid::Mm(0.5),
            Self::FullTemplate => NormGrid::Full,
            Self::Custom => NormGrid::Mm(DEFAULT_NORM_ISOTROPIC_MM),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Template => "Template resolution (recommended)",
            Self::Mm1p0 => "1.0 mm",
            Self::Mm0p8 => "0.8 mm",
            Self::Mm0p5 => "0.5 mm",
            Self::FullTemplate => "Full template grid, no resampling",
            Self::Custom => "Custom…",
        }
    }

    pub fn label_for(self, voxel: Option<f64>) -> String {
        match (self, voxel) {
            (Self::Template, Some(v)) if v.is_finite() && v > 0.0 => {
                format!("Template resolution ({} mm, recommended)", mm_text(v))
            }
            (Self::FullTemplate, Some(v)) if v.is_finite() && v > 0.0 => {
                format!("Full template grid ({} mm, no resampling)", mm_text(v))
            }
            _ => self.label().to_owned(),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Template => {
                "The template's own voxel size. When the graphics card cannot fit that grid, the \
                 run steps to the finest coarser grid that fits and says so."
            }
            Self::FullTemplate => {
                "No resampling at all: the registration runs on the template grid exactly as \
                 stored, also when its voxels are not cubes."
            }
            Self::Custom => "Any isotropic voxel size, in millimetres.",
            Self::Mm1p0 | Self::Mm0p8 | Self::Mm0p5 => {
                "A fixed isotropic voxel size. It is used as chosen, also when the graphics \
                 card cannot fit it."
            }
        }
    }

    pub fn finer_than(self, voxel: Option<f64>) -> bool {
        match (self, voxel) {
            (Self::Mm1p0 | Self::Mm0p8 | Self::Mm0p5, Some(v)) => {
                self.value().mm().is_some_and(|mm| finer_than_voxel(mm, v))
            }
            _ => false,
        }
    }
}

const FINER_EPS_MM: f64 = 1e-6;

fn mm_text(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{r:?}")
}

pub fn finer_than_voxel(mm: f64, voxel: f64) -> bool {
    let voxel = (voxel * 100.0).round() / 100.0;
    mm < voxel - FINER_EPS_MM
}

pub fn norm_custom_range(voxel: Option<f64>) -> (f64, f64) {
    match voxel {
        Some(v) if v.is_finite() && v > 0.0 => (0.3_f64.max(v), 3.0_f64.max(v)),
        _ => (0.3, 3.0),
    }
}

pub fn finer_disabled_hover(voxel: f64) -> String {
    format!(
        "Finer than the template's {} mm voxels: the template would only be interpolated, which adds no detail but costs memory and time.",
        mm_text(voxel)
    )
}

pub fn finer_selection_warning(voxel: f64) -> String {
    format!(
        "Finer than the template's {} mm voxels: the template is only interpolated, which adds no detail but costs memory and time.",
        mm_text(voxel)
    )
}

pub fn stepped_grid_text(template_mm: f64, effective_mm: f64) -> String {
    format!(
        "Registration grid stepped to {} mm: the graphics card cannot fit the template's {} mm grid.",
        mm_text(effective_mm),
        mm_text(template_mm)
    )
}

pub fn grid_size_text(dims: [usize; 3]) -> String {
    let total = dims.iter().map(|&d| d as f64).product::<f64>();
    format!(
        "Grid: {} × {} × {} voxels ({:.1} million)",
        dims[0],
        dims[1],
        dims[2],
        total / 1.0e6
    )
}

/// Which engine runs the normalization registration (`--reg-backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RegistrationEngine {
    #[default]
    Auto,
    Gpu,
    Ants,
}

impl RegistrationEngine {
    pub fn value(self) -> RegBackend {
        match self {
            Self::Auto => RegBackend::Auto,
            Self::Gpu => RegBackend::Wgpu,
            Self::Ants => RegBackend::Ants,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (GPU when available)",
            Self::Gpu => "GPU (wgpu)",
            Self::Ants => "ANTs (CPU)",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Auto => {
                "Use the graphics card when a hardware adapter passes the start-up self-test, \
                 otherwise ANTs on the processor."
            }
            Self::Gpu => {
                "Always use the graphics card. The step fails with an explanation when no \
                 usable adapter exists. Needs no ANTs programs for this step."
            }
            Self::Ants => "Always use the ANTs programs on the processor (slow, well tested).",
        }
    }
}

/// The "Method" text shown on the registration-driven step cards.
///
/// The registration engine applies to Preprocess, Coregister **and** Normalize,
/// so the card must say which engine will actually run rather than hardcoding
/// ANTs. Pure so the mapping is testable without a UI.
pub fn registration_method_label(stage: Stage, engine: RegistrationEngine) -> String {
    let base = match stage {
        Stage::Preprocess => "N4 bias field correction",
        Stage::Coregister => "Rigid registration (multi-start)",
        _ => return String::new(),
    };
    let engine = match engine {
        RegistrationEngine::Auto => "Auto",
        RegistrationEngine::Gpu => "GPU (wgpu)",
        RegistrationEngine::Ants => "ANTs (CPU)",
    };
    format!("{base} — {engine}")
}

/// Whether the selected engine falls back to the bundled ANTs programs when the
/// graphics card cannot be used. `Auto` with the probe still running (`None`) is
/// assumed to have the GPU, so no false "install ANTs" warning appears during it.
pub fn ants_programs_needed(engine: RegistrationEngine, gpu_available: Option<bool>) -> bool {
    match engine {
        RegistrationEngine::Ants => true,
        RegistrationEngine::Gpu => false,
        RegistrationEngine::Auto => gpu_available == Some(false),
    }
}

/// Why the Normalize step cannot start with the chosen engine, if it cannot.
pub fn normalize_engine_blocker(
    engine: RegistrationEngine,
    r: &PipelineReadiness,
) -> Option<&'static str> {
    match (engine, r.gpu_available) {
        (RegistrationEngine::Ants, _) if !r.has_ants_registration => {
            Some("antsRegistration not found")
        }
        (RegistrationEngine::Gpu, Some(false)) => Some("No usable graphics card found"),
        (RegistrationEngine::Auto, Some(false)) if !r.has_ants_registration => {
            Some("No usable graphics card and antsRegistration not found")
        }
        (RegistrationEngine::Auto, None) | (RegistrationEngine::Gpu, None) => None,
        _ => None,
    }
}

/// Why Preprocess cannot start with the chosen engine. N4 bias-field correction
/// runs on the GPU under `wgpu` (`specs/02` §4.3), so a missing ANTs program does
/// not block it there — the pipeline's own prerequisite check agrees
/// (`prereqs.rs`).
pub fn preprocess_engine_blocker(
    engine: RegistrationEngine,
    r: &PipelineReadiness,
) -> Option<&'static str> {
    match (engine, r.gpu_available) {
        (RegistrationEngine::Ants, _) if !r.has_n4_bias_correction => {
            Some("N4BiasFieldCorrection not found")
        }
        (RegistrationEngine::Gpu, Some(false)) => Some("No usable graphics card found"),
        (RegistrationEngine::Auto, Some(false)) if !r.has_n4_bias_correction => {
            Some("No usable graphics card and N4BiasFieldCorrection not found")
        }
        _ => None,
    }
}

/// Why Coregister cannot start with the chosen engine. The multi-start rigid
/// registration runs on the GPU under `wgpu` (`specs/03` §5.1), so a missing
/// `antsRegistration` does not block it there.
pub fn coregister_engine_blocker(
    engine: RegistrationEngine,
    r: &PipelineReadiness,
) -> Option<&'static str> {
    match (engine, r.gpu_available) {
        (RegistrationEngine::Ants, _) if !r.has_ants_registration => {
            Some("antsRegistration not found")
        }
        (RegistrationEngine::Gpu, Some(false)) => Some("No usable graphics card found"),
        (RegistrationEngine::Auto, Some(false)) if !r.has_ants_registration => {
            Some("No usable graphics card and antsRegistration not found")
        }
        _ => None,
    }
}

pub fn precoreg_engine_blocker(
    engine: RegistrationEngine,
    r: &PipelineReadiness,
) -> Option<&'static str> {
    match (engine, r.gpu_available) {
        (RegistrationEngine::Ants, _) if !r.has_ants_registration => {
            Some("antsRegistration not found (needed by the pre-alignment to the template)")
        }
        (RegistrationEngine::Gpu, Some(false)) => {
            Some("No usable graphics card found (needed by the pre-alignment to the template)")
        }
        (RegistrationEngine::Auto, Some(false)) if !r.has_ants_registration => Some(
            "No usable graphics card and antsRegistration not found (needed by the pre-alignment \
             to the template)",
        ),
        _ => None,
    }
}

pub fn precoreg_needed(stages: &[Stage], r: &PipelineReadiness) -> bool {
    stages.contains(&Stage::Coregister)
        || (stages.contains(&Stage::Normalize) && !r.has_raw_postop())
}

pub fn stages_with_precoreg(stages: &[Stage], r: &PipelineReadiness) -> Vec<Stage> {
    let mut out = stages.to_vec();
    if precoreg_needed(stages, r) && !out.contains(&Stage::Precoreg) {
        out.push(Stage::Precoreg);
        let order = Stage::default_sequence();
        out.sort_by_key(|s| order.iter().position(|o| o == s));
    }
    out
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ThreadsCompat {
    Count(u32),
    Name(String),
}

/// Reads the thread cap, accepting the names an earlier version saved (`Auto`, `One`, `Two`,
/// `Four`, `Eight`) next to a plain count or `null`.
fn deserialize_threads<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<ThreadsCompat>::deserialize(deserializer)? {
        None => None,
        Some(ThreadsCompat::Count(n)) => Some(n.max(1)),
        Some(ThreadsCompat::Name(name)) => match name.as_str() {
            "One" => Some(1),
            "Two" => Some(2),
            "Four" => Some(4),
            "Eight" => Some(8),
            _ => None,
        },
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostopModality {
    #[default]
    Auto,
    Ct,
    Mri,
}

impl PostopModality {
    pub const ALL: [PostopModality; 3] = [Self::Auto, Self::Ct, Self::Mri];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (CT when both exist)",
            Self::Ct => "CT",
            Self::Mri => "MRI",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetalContrast {
    Dark,
    Bright,
}

impl MetalContrast {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark metal artefact",
            Self::Bright => "Bright metal artefact",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Dark => {
                "The lead looks darker than the surrounding tissue in the post-operative MRI \
                 (the usual case). A post-operative CT ignores this setting."
            }
            Self::Bright => {
                "The lead looks brighter than the surrounding tissue in the post-operative MRI. \
                 A post-operative CT ignores this setting."
            }
        }
    }
}

/// Everything the user can adjust in the Pipeline panel. Persisted with the rest
/// of the application state, so a restart restores the previous run setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StageSettings {
    pub run_preprocess: bool,
    pub run_coreg: bool,
    pub run_norm: bool,
    pub run_reconstruct: bool,
    pub run_orientation: bool,
    pub run_vat: bool,
    pub run_visualize: bool,
    pub overwrite_results: bool,
    pub stop_on_error: bool,
    pub open_viewer_after_reconstruct: bool,
    pub recon_method: ReconMethod,
    pub el_model: Option<String>,
    /// Take each subject's electrode model from its `uiprefs.mat` instead of `el_model`.
    pub el_model_from_subject: bool,
    pub norm_resolution: NormResolution,
    #[serde(default = "default_norm_custom_mm")]
    pub norm_custom_mm: f64,
    pub preprocess_engine: RegistrationEngine,
    pub coregister_engine: RegistrationEngine,
    pub normalize_engine: RegistrationEngine,
    /// The single engine setting older versions saved; read once by `migrate_legacy_engine`.
    #[serde(default, skip_serializing, rename = "registration_engine")]
    #[doc(hidden)]
    pub legacy_registration_engine: Option<RegistrationEngine>,
    /// Register T1w + T2w against the template together when the subject has a
    /// non-anchor T2w and a T2 template resolves. Default on, matching the CLI.
    pub norm_multispectral: bool,
    /// Most processor threads the registration steps may use; `None` means all cores.
    #[serde(deserialize_with = "deserialize_threads")]
    pub ants_threads: Option<u32>,
    pub metal_contrast: MetalContrast,
    /// When registrations are reviewed; older files saved a `review_registrations` flag.
    #[serde(
        alias = "review_registrations",
        deserialize_with = "deserialize_review_mode"
    )]
    pub review_mode: ReviewMode,
    pub postop_modality: PostopModality,
    /// Folder of further Lead-DBS atlases listed in the 3D scene (`specs/11` §1); `None` = only
    /// the bundled atlases.
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// Folder that holds the downloaded connectomes (`specs/15` §3.1); `None` = the default
    /// per-user folder.
    pub connectomes_dir: Option<PathBuf>,
    /// A Lead-DBS connectomes folder that is read where it is (`specs/15` §3.1, `specs/17` §3.1);
    /// takes precedence over `DBSLOC_LEADDBS_CONNECTOMES_DIR`; `None` = use the environment value.
    pub leaddbs_connectomes_dir: Option<PathBuf>,
}

fn deserialize_review_mode<'de, D>(deserializer: D) -> Result<ReviewMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Compat {
        Flag(bool),
        Mode(ReviewMode),
    }
    Ok(match Compat::deserialize(deserializer)? {
        Compat::Flag(true) => ReviewMode::DuringRun,
        Compat::Flag(false) => ReviewMode::Off,
        Compat::Mode(mode) => mode,
    })
}

fn default_norm_custom_mm() -> f64 {
    1.5
}

impl Default for StageSettings {
    fn default() -> Self {
        Self {
            run_preprocess: true,
            run_coreg: true,
            run_norm: true,
            run_reconstruct: true,
            run_orientation: true,
            run_vat: true,
            run_visualize: true,
            overwrite_results: false,
            stop_on_error: false,
            open_viewer_after_reconstruct: true,
            recon_method: ReconMethod::Auto,
            el_model: None,
            el_model_from_subject: false,
            norm_resolution: NormResolution::Template,
            norm_custom_mm: default_norm_custom_mm(),
            preprocess_engine: RegistrationEngine::Auto,
            coregister_engine: RegistrationEngine::Auto,
            normalize_engine: RegistrationEngine::Auto,
            legacy_registration_engine: None,
            norm_multispectral: true,
            ants_threads: None,
            metal_contrast: MetalContrast::Dark,
            review_mode: ReviewMode::DuringRun,
            postop_modality: PostopModality::Auto,
            leaddbs_atlases_dir: None,
            connectomes_dir: None,
            leaddbs_connectomes_dir: None,
        }
    }
}

impl StageSettings {
    /// Seeds the three per-step engines from the single engine an older version saved.
    pub fn migrate_legacy_engine(&mut self) {
        if let Some(engine) = self.legacy_registration_engine.take() {
            self.preprocess_engine = engine;
            self.coregister_engine = engine;
            self.normalize_engine = engine;
        }
    }

    /// The engine chosen for `stage`; steps without a registration use `Auto`.
    pub fn engine_for(&self, stage: Stage) -> RegistrationEngine {
        match stage {
            Stage::Preprocess => self.preprocess_engine,
            Stage::Coregister => self.coregister_engine,
            Stage::Normalize => self.normalize_engine,
            _ => RegistrationEngine::Auto,
        }
    }

    pub fn norm_grid(&self) -> NormGrid {
        match self.norm_resolution {
            NormResolution::Custom if self.norm_custom_mm.is_finite() => {
                NormGrid::Mm((self.norm_custom_mm * 100.0).round() / 100.0)
            }
            NormResolution::Custom => NormGrid::Mm(default_norm_custom_mm()),
            fixed => fixed.value(),
        }
    }

    pub fn selected(&self, stage: Stage) -> bool {
        match stage {
            Stage::Preprocess => self.run_preprocess,
            Stage::Coregister => self.run_coreg,
            Stage::Normalize => self.run_norm,
            Stage::Reconstruct => self.run_reconstruct,
            Stage::Orientation => self.run_orientation,
            Stage::Vat => self.run_vat,
            Stage::Visualize => self.run_visualize,
            Stage::Bids | Stage::Precoreg | Stage::Brainshift => false,
        }
    }

    pub fn set_selected(&mut self, stage: Stage, on: bool) {
        match stage {
            Stage::Preprocess => self.run_preprocess = on,
            Stage::Coregister => self.run_coreg = on,
            Stage::Normalize => self.run_norm = on,
            Stage::Reconstruct => self.run_reconstruct = on,
            Stage::Orientation => self.run_orientation = on,
            Stage::Vat => self.run_vat = on,
            Stage::Visualize => self.run_visualize = on,
            Stage::Bids | Stage::Precoreg | Stage::Brainshift => {}
        }
    }

    /// The settings a run of exactly `stage` is judged by: only that step
    /// selected, so what an earlier step of a larger run would produce is not
    /// counted as available.
    pub fn only(&self, stage: Stage) -> StageSettings {
        let mut s = self.clone();
        s.set_all(false);
        s.set_selected(stage, true);
        s
    }

    pub fn set_all(&mut self, on: bool) {
        for stage in Stage::work_sequence() {
            self.set_selected(*stage, on);
        }
    }

    pub fn selected_stages(&self) -> Vec<Stage> {
        Stage::work_sequence()
            .iter()
            .copied()
            .filter(|s| self.selected(*s))
            .collect()
    }

    /// Selected steps whose inputs neither exist on disk nor can be produced by
    /// another selected step in the same run.
    ///
    /// This is a **query**: the user's selection is never silently changed, so a
    /// tick they made is still there once they fix the prerequisite.
    pub fn unreachable_stages(&self, r: &PipelineReadiness) -> Vec<Stage> {
        let reach = Reach::new(self, r);
        let norm_reachable = reach.norm;
        let recon_reachable = reach.recon;
        let mut out = Vec::new();
        if self.run_preprocess && !r.has_raw_anat && !r.has_preprocess_nifti {
            out.push(Stage::Preprocess);
        }
        if self.run_coreg && !r.has_coreg_warped && (!r.has_raw_postop() || !r.template_exists) {
            out.push(Stage::Coregister);
        }
        if self.run_norm && !r.has_norm_nifti && (!r.template_exists || !reach.norm_inputs) {
            out.push(Stage::Normalize);
        }
        if self.run_reconstruct && !reach.norm_postop && !r.has_reconstruction_json {
            out.push(Stage::Reconstruct);
        }
        if self.run_orientation && !recon_reachable {
            out.push(Stage::Orientation);
        }
        if self.run_vat && !norm_reachable {
            out.push(Stage::Vat);
        }
        if self.run_visualize && (!recon_reachable || !norm_reachable) {
            out.push(Stage::Visualize);
        }
        out
    }

    /// Untick exactly the steps [`Self::unreachable_stages`] reports. Bound to an
    /// explicit button, so the user — not the application — drops their selection.
    pub fn deselect_unreachable(&mut self, r: &PipelineReadiness) {
        for stage in self.unreachable_stages(r) {
            self.set_selected(stage, false);
        }
    }

    pub fn selected_runnable_stages(&self, readiness: &PipelineReadiness) -> Vec<Stage> {
        self.selected_stages()
            .into_iter()
            .filter(|stage| self.overwrite_results || !readiness.stage_output_exists(*stage))
            .collect()
    }

    /// Steps that already have output on disk and would be replaced.
    pub fn stages_that_would_be_overwritten(&self, readiness: &PipelineReadiness) -> Vec<Stage> {
        self.selected_stages()
            .into_iter()
            .filter(|s| readiness.stage_output_exists(*s))
            .collect()
    }

    pub fn selection_blockers(
        &self,
        readiness: &PipelineReadiness,
        runnable_stages: &[Stage],
    ) -> Vec<String> {
        let mut blockers = Vec::new();
        if !readiness.has_bids_dir {
            blockers.push("Choose a readable input directory.".to_owned());
        }
        if !readiness.has_subject {
            blockers.push("Select a subject.".to_owned());
        }
        for stage in self.unreachable_stages(readiness) {
            let msg = match stage {
                Stage::Coregister if !readiness.has_raw_postop() => {
                    "Coregistration needs a post-operative CT or MRI in this subject.".to_owned()
                }
                Stage::Coregister => "Coregistration needs a readable template NIfTI file for \
                     the pre-alignment to the template."
                    .to_owned(),
                Stage::Normalize if !readiness.template_exists => {
                    "Normalize needs a readable template NIfTI file.".to_owned()
                }
                Stage::Normalize => "Normalize needs the coregistered post-operative image — \
                     also tick Coregister, or run it first."
                    .to_owned(),
                Stage::Reconstruct => {
                    "Reconstruction needs a normalized post-operative image — also tick \
                     Normalize, or run it first."
                        .to_owned()
                }
                Stage::Orientation => {
                    "Directional orientation needs reconstruction output.".to_owned()
                }
                Stage::Vat => "The volume of activated tissue needs normalized anatomy — also \
                     tick Normalize, or run it first."
                    .to_owned(),
                Stage::Visualize => {
                    "The electrode image needs both normalized anatomy and reconstruction output."
                        .to_owned()
                }
                Stage::Preprocess => {
                    "Preprocessing needs a raw anatomy image in this subject.".to_owned()
                }
                Stage::Bids | Stage::Precoreg | Stage::Brainshift => continue,
            };
            blockers.push(msg);
        }
        if let Some(b) = self.electrode_blocker() {
            blockers.push(b);
        }
        if self.selected_stages().is_empty() {
            blockers.push("Select at least one pipeline step.".to_owned());
        } else if runnable_stages.is_empty() {
            blockers.push(
                "Every selected step already has output. Tick \"Rerun all selected steps\" \
                 to run them again."
                    .to_owned(),
            );
        }
        blockers
    }

    fn electrode_blocker(&self) -> Option<String> {
        (self.run_reconstruct && self.el_model.is_none() && !self.el_model_from_subject).then(
            || {
                "Reconstruction needs an electrode model — pick one, or \"From each subject's \
             settings\", in the Reconstruct options."
                    .to_owned()
            },
        )
    }

    /// What stops a run of several subjects before it starts. Everything that depends on
    /// one subject's files is judged per subject when its turn comes.
    pub fn batch_blockers(&self, has_bids_dir: bool) -> Vec<String> {
        let mut blockers = Vec::new();
        if !has_bids_dir {
            blockers.push("Choose a readable input directory.".to_owned());
        }
        if let Some(b) = self.electrode_blocker() {
            blockers.push(b);
        }
        if self.selected_stages().is_empty() {
            blockers.push("Select at least one pipeline step.".to_owned());
        }
        blockers
    }

    /// The electrode model a run passes on: `None` lets the pipeline read each subject's
    /// `uiprefs.mat`.
    pub fn el_model_for_run(&self) -> Option<String> {
        if self.el_model_from_subject {
            None
        } else {
            self.el_model.clone()
        }
    }

    pub fn upstream_review_warnings(
        &self,
        readiness: &PipelineReadiness,
        stage: Stage,
    ) -> Vec<UpstreamReviewWarning> {
        if !self.selected(stage) {
            return Vec::new();
        }
        let runnable = self.selected_runnable_stages(readiness);
        [Stage::Coregister, Stage::Normalize]
            .into_iter()
            .filter(|up| up.invalidated_dependents().contains(&stage))
            .filter(|up| readiness.stage_output_exists(*up) && !runnable.contains(up))
            .filter_map(|up| {
                let state = readiness.registration_review(up)?;
                (state != RegistrationReview::Approved).then_some(UpstreamReviewWarning {
                    upstream: up,
                    state,
                })
            })
            .collect()
    }

    /// The pure mapping from the UI settings to the pipeline's run configuration.
    ///
    /// Returns `None` when there is nothing to run. No filesystem or pipeline
    /// side effects: the front end pairs this with the resolved template paths
    /// and hands the [`RunRequest`] to the pipeline. `normalization_template_t2`
    /// is the optional T2 template path from the application state.
    pub fn run_request(
        &self,
        stages: &[Stage],
        normalization_template_t2: Option<&str>,
    ) -> Option<RunRequest> {
        if stages.is_empty() {
            return None;
        }
        Some(RunRequest {
            stages: stages.to_vec(),
            overwrite_approved: self.overwrite_results,
            stop_on_error: self.stop_on_error,
            open_viewer_after_reconstruct: self.open_viewer_after_reconstruct,
            recon_method: self.recon_method,
            recon_bright_metal: self.metal_contrast == MetalContrast::Bright,
            el_model: self.el_model_for_run(),
            norm_grid: self.norm_grid(),
            normalization_template_t2: normalization_template_t2.map(str::to_owned),
            norm_multispectral: self.norm_multispectral,
            reg_backend_preprocess: Some(self.preprocess_engine.value()),
            reg_backend_coregister: Some(self.coregister_engine.value()),
            reg_backend_normalize: Some(self.normalize_engine.value()),
            ants_max_threads: self.ants_threads,
            review_registrations: self.review_mode != ReviewMode::Off,
            review_mode: self.review_mode,
            postop_modality: self.postop_modality,
            leaddbs_atlases_dir: self.leaddbs_atlases_dir.clone(),
            vat_labels: Vec::new(),
        })
    }
}

/// A fully resolved run configuration handed to the pipeline thread.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub stages: Vec<Stage>,
    pub overwrite_approved: bool,
    pub stop_on_error: bool,
    pub open_viewer_after_reconstruct: bool,
    pub recon_method: ReconMethod,
    pub recon_bright_metal: bool,
    /// Electrode catalogue name; `None` is refused before the run starts rather
    /// than guessing a lead.
    pub el_model: Option<String>,
    pub norm_grid: NormGrid,
    /// T2 template for the multispectral SyN, when the user set one.
    pub normalization_template_t2: Option<String>,
    /// Register T1w + T2w together (SyN); `false` forces T1w-only.
    pub norm_multispectral: bool,
    pub reg_backend_preprocess: Option<RegBackend>,
    pub reg_backend_coregister: Option<RegBackend>,
    pub reg_backend_normalize: Option<RegBackend>,
    pub ants_max_threads: Option<u32>,
    /// Registrations are reviewed (a single-subject run reviews during the run).
    pub review_registrations: bool,
    pub review_mode: ReviewMode,
    pub postop_modality: PostopModality,
    pub leaddbs_atlases_dir: Option<PathBuf>,
    /// Stimulation labels the volume of activated tissue step computes; empty = every label.
    pub vat_labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamReviewWarning {
    pub upstream: Stage,
    pub state: RegistrationReview,
}

impl UpstreamReviewWarning {
    pub fn text(&self) -> String {
        let what = match self.upstream {
            Stage::Coregister => "coregistration",
            _ => "normalization",
        };
        match self.state {
            RegistrationReview::Rejected => format!("Uses a {what} that was rejected"),
            _ => format!("Uses a {what} that was not reviewed"),
        }
    }
}

impl UpstreamReviewWarning {
    pub fn hover(&self) -> &'static str {
        match self.state {
            RegistrationReview::Rejected => {
                "You rejected this registration. Results that build on it may be wrong. Review it again or rerun the step."
            }
            _ => {
                "Nobody has approved this registration yet. Results that build on it may be wrong."
            }
        }
    }
}

pub const REVIEW_REGISTRATIONS_LABEL: &str = "Review registrations during the run";
pub const REVIEW_REGISTRATIONS_HOVER: &str = "After Coregister and after Normalize the result is shown for review and the run waits. Approve to continue, reject to stop the run and mark the step for rework.";
pub const REVIEW_MODE_LABEL: &str = "Review registrations";
pub const REVIEW_MODE_HOVER: &str = "When the results of Coregister and Normalize are shown for review. During the run: the run waits for your verdict, subject by subject. After all subjects (runs of several subjects): a subject stops at its first registration that needs review, the other subjects run on, and the reviews open one after another at the end; approving one runs that subject's remaining steps. Off: no review.";
pub const EL_MODEL_FROM_SUBJECT_LABEL: &str = "From each subject's settings";
pub const EL_MODEL_FROM_SUBJECT_HOVER: &str = "Use the electrode model stored in each subject's settings file (prefs/sub-<id>_desc-uiprefs.mat). A subject without one fails its reconstruction with a message.";

pub struct StageMeta {
    pub stage: Stage,
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

pub const STAGE_META: [StageMeta; 7] = [
    StageMeta {
        stage: Stage::Preprocess,
        id: "preprocess",
        title: "Preprocess MRI / CT",
        description: "Bias-correct and prepare the raw anatomy images.",
    },
    StageMeta {
        stage: Stage::Coregister,
        id: "coregister",
        title: "Coregister post-operative image",
        description: "Align the post-operative CT or MRI to the pre-operative anchor image.",
    },
    StageMeta {
        stage: Stage::Normalize,
        id: "normalize",
        title: "Normalize to template",
        description: "Warp the anatomy into template (MNI) space.",
    },
    StageMeta {
        stage: Stage::Reconstruct,
        id: "reconstruct",
        title: "Reconstruct electrode",
        description: "Find the lead trajectory and the contact coordinates.",
    },
    StageMeta {
        stage: Stage::Orientation,
        id: "orientation",
        title: "Estimate directional orientation",
        description: "Directional lead rotation — experimental; Boston and Abbott leads only.",
    },
    StageMeta {
        stage: Stage::Vat,
        id: "vat",
        title: "Volume of activated tissue",
        description: "Compute the electric field and the volume of activated tissue from the saved stimulation settings; skipped when there are none.",
    },
    StageMeta {
        stage: Stage::Visualize,
        id: "electrode_image",
        title: "Write electrode image",
        description: "Write the 3D scene manifest that opens the electrode in the viewer.",
    },
];

struct Reach {
    coreg: bool,
    norm_inputs: bool,
    norm: bool,
    norm_postop: bool,
    recon: bool,
}

impl Reach {
    fn new(s: &StageSettings, r: &PipelineReadiness) -> Self {
        let postop = r.has_raw_postop();
        let coreg = r.has_coreg_warped || (s.run_coreg && postop && r.template_exists);
        let norm_inputs = !postop || coreg;
        let norm = r.has_norm_nifti || (s.run_norm && r.template_exists && norm_inputs);
        let norm_postop =
            r.has_norm_postop() || (s.run_norm && r.template_exists && postop && coreg);
        let recon = r.has_reconstruction_json || (s.run_reconstruct && norm_postop);
        Self {
            coreg,
            norm_inputs,
            norm,
            norm_postop,
            recon,
        }
    }
}

pub fn stage_status_detail(
    stage: Stage,
    s: &StageSettings,
    r: &PipelineReadiness,
) -> (StageState, String) {
    let (state, text) = stage_status(stage, s, r);
    let count = r.stimulation_labels.len();
    if stage == Stage::Vat && state == StageState::Ready && !r.has_vat_preview && count > 0 {
        let plural = if count == 1 { "" } else { "s" };
        return (
            state,
            format!("Computes the VTA of {count} saved stimulation{plural}"),
        );
    }
    (state, text.to_owned())
}

pub fn stage_status(
    stage: Stage,
    s: &StageSettings,
    r: &PipelineReadiness,
) -> (StageState, &'static str) {
    if !r.has_bids_dir || !r.has_subject {
        return (StageState::Blocked, "No subject selected");
    }
    let reach = Reach::new(s, r);
    let postop = r.has_raw_postop();

    match stage {
        Stage::Preprocess => {
            if r.has_preprocess_nifti {
                (StageState::Ready, "Preprocessed anatomy already present")
            } else if !r.has_raw_anat {
                (StageState::Blocked, "No raw anatomy image in this subject")
            } else if let Some(why) = preprocess_engine_blocker(s.preprocess_engine, r) {
                (StageState::Blocked, why)
            } else {
                (StageState::Ready, "Raw anatomy found")
            }
        }
        Stage::Coregister => {
            if r.has_coreg_warped {
                (StageState::Ready, "Coregistered anatomy already present")
            } else if !postop {
                (
                    StageState::Blocked,
                    "No post-operative CT or MRI in this subject",
                )
            } else if !r.template_exists {
                (
                    StageState::Blocked,
                    "Template file missing (needed by the pre-alignment to the template)",
                )
            } else if let Some(why) = coregister_engine_blocker(s.coregister_engine, r)
                .or_else(|| precoreg_engine_blocker(s.preprocess_engine, r))
            {
                (StageState::Blocked, why)
            } else if r.has_raw_postop_ct {
                (StageState::Ready, "Post-operative CT found")
            } else {
                (StageState::Ready, "Post-operative MRI found")
            }
        }
        Stage::Normalize => {
            if r.has_norm_nifti {
                (StageState::Ready, "Normalized anatomy already present")
            } else if !r.template_exists {
                (StageState::Blocked, "Template file missing")
            } else if let Some(why) = normalize_engine_blocker(s.normalize_engine, r) {
                (StageState::Blocked, why)
            } else if let Some(why) =
                precoreg_engine_blocker(s.preprocess_engine, r).filter(|_| !postop)
            {
                (StageState::Blocked, why)
            } else if postop && !r.has_coreg_warped {
                if reach.coreg {
                    (
                        StageState::Ready,
                        "Will use the output of Coregister in this run",
                    )
                } else {
                    (
                        StageState::NeedsUpstream,
                        "Needs the coregistered post-operative image",
                    )
                }
            } else {
                (StageState::Ready, "Template file found")
            }
        }
        Stage::Reconstruct => {
            if r.has_reconstruction_json {
                (StageState::Ready, "Reconstruction already present")
            } else if r.has_norm_postop() {
                (
                    StageState::Ready,
                    "Normalized post-operative image available",
                )
            } else if reach.norm_postop {
                (
                    StageState::Ready,
                    "Will use the output of Normalize in this run",
                )
            } else {
                (
                    StageState::NeedsUpstream,
                    "Needs a normalized post-operative image",
                )
            }
        }
        Stage::Orientation => {
            if r.has_orientation_json {
                (StageState::Ready, "Orientation already present")
            } else if r.has_reconstruction_json {
                (StageState::Ready, "Reconstruction available")
            } else if reach.recon {
                (
                    StageState::Ready,
                    "Will use the output of Reconstruct in this run",
                )
            } else {
                (StageState::NeedsUpstream, "Needs reconstruction output")
            }
        }
        Stage::Vat => {
            if r.has_vat_preview {
                (StageState::Ready, "Output already present")
            } else if reach.norm {
                (
                    StageState::Ready,
                    "Uses the saved stimulation settings; skipped when there are none",
                )
            } else {
                (StageState::NeedsUpstream, "Needs normalized anatomy")
            }
        }
        Stage::Bids | Stage::Precoreg | Stage::Brainshift => (StageState::Ready, ""),
        Stage::Visualize => {
            if r.has_scene_manifest {
                (StageState::Ready, "Scene manifest already present")
            } else if !reach.norm {
                (StageState::NeedsUpstream, "Needs normalized anatomy")
            } else if reach.recon {
                (StageState::Ready, "Reconstruction available in this run")
            } else {
                (StageState::NeedsUpstream, "Needs reconstruction output")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn default_settings_select_the_work_sequence() {
        let s = StageSettings::default();
        assert_eq!(s.selected_stages(), Stage::work_sequence().to_vec());
        assert!(s.norm_multispectral);
        assert_eq!(s.review_mode, ReviewMode::DuringRun);
        assert!(!s.el_model_from_subject);
        assert_eq!(s.norm_resolution, NormResolution::Template);
        assert_eq!(s.norm_resolution.value(), NormGrid::Template);
        assert_eq!(s.norm_grid(), NormGrid::Template);
        assert_eq!(s.normalize_engine.value(), RegBackend::Auto);
    }

    #[test]
    fn only_keeps_exactly_one_step_selected() {
        let s = StageSettings::default();
        let only = s.only(Stage::Normalize);
        assert_eq!(only.selected_stages(), vec![Stage::Normalize]);
        assert!(s.run_preprocess, "the original selection is untouched");
    }

    #[test]
    fn run_request_is_none_for_an_empty_stage_list() {
        let s = StageSettings::default();
        assert!(s.run_request(&[], None).is_none());
    }

    #[test]
    fn run_request_carries_the_settings_and_t2_template() {
        let s = StageSettings {
            recon_method: ReconMethod::Pacer,
            metal_contrast: MetalContrast::Bright,
            norm_multispectral: false,
            el_model: Some("any".to_owned()),
            ..StageSettings::default()
        };
        let req = s
            .run_request(&[Stage::Reconstruct], Some("tpl_t2.nii.gz"))
            .expect("a non-empty stage list builds a request");
        assert_eq!(req.stages, vec![Stage::Reconstruct]);
        assert_eq!(req.recon_method, ReconMethod::Pacer);
        assert!(req.recon_bright_metal);
        assert!(!req.norm_multispectral);
        assert_eq!(req.el_model.as_deref(), Some("any"));
        assert_eq!(
            req.normalization_template_t2.as_deref(),
            Some("tpl_t2.nii.gz")
        );
    }

    #[test]
    fn review_registrations_loads_true_from_old_json() {
        let empty: StageSettings = serde_json::from_str("{}").expect("empty");
        assert_eq!(empty.review_mode, ReviewMode::DuringRun);
        let old: StageSettings =
            serde_json::from_str(r#"{"run_coreg": false, "stop_on_error": true}"#).expect("old");
        assert_eq!(old.review_mode, ReviewMode::DuringRun);
        assert!(!old.run_coreg);
        let off: StageSettings =
            serde_json::from_str(r#"{"review_registrations": false}"#).expect("off");
        assert_eq!(off.review_mode, ReviewMode::Off);
        let on: StageSettings =
            serde_json::from_str(r#"{"review_registrations": true}"#).expect("on");
        assert_eq!(on.review_mode, ReviewMode::DuringRun);
    }

    #[test]
    fn the_review_mode_survives_a_round_trip() {
        for mode in [ReviewMode::DuringRun, ReviewMode::AfterAll, ReviewMode::Off] {
            let s = StageSettings {
                review_mode: mode,
                el_model_from_subject: true,
                ..StageSettings::default()
            };
            let text = serde_json::to_string(&s).expect("serialize");
            assert!(text.contains("\"review_mode\""), "{text}");
            assert!(!text.contains("review_registrations"), "{text}");
            let back: StageSettings = serde_json::from_str(&text).expect("load");
            assert_eq!(back.review_mode, mode);
            assert!(back.el_model_from_subject);
        }
    }

    #[test]
    fn subject_electrode_models_lift_the_blocker_and_pass_no_model() {
        let r = full_readiness();
        let mut s = StageSettings::default().only(Stage::Reconstruct);
        s.overwrite_results = true;
        s.el_model = Some("any".to_owned());
        s.el_model_from_subject = true;
        let runnable = s.selected_runnable_stages(&r);
        assert!(!s
            .selection_blockers(&r, &runnable)
            .iter()
            .any(|b| b.contains("electrode")));
        let req = s.run_request(&runnable, None).expect("request");
        assert_eq!(req.el_model, None);
        s.el_model_from_subject = false;
        s.el_model = None;
        assert!(s
            .batch_blockers(true)
            .iter()
            .any(|b| b.contains("electrode")));
        assert!(s.batch_blockers(false).len() >= 2);
        s.el_model = Some("any".to_owned());
        assert!(s.batch_blockers(true).is_empty());
    }

    #[test]
    fn the_removed_verbose_output_setting_still_loads() {
        let s: StageSettings =
            serde_json::from_str(r#"{"verbose_tool_output": true, "stop_on_error": true}"#)
                .expect("old persisted json loads");
        assert!(s.stop_on_error);
        let text = serde_json::to_string(&s).expect("serialize");
        assert!(!text.contains("verbose_tool_output"));
    }

    #[test]
    fn run_request_carries_review_registrations() {
        let mut s = StageSettings::default();
        let on = s.run_request(&[Stage::Coregister], None).expect("request");
        assert!(on.review_registrations);
        assert_eq!(on.review_mode, ReviewMode::DuringRun);
        s.review_mode = ReviewMode::AfterAll;
        let after = s.run_request(&[Stage::Coregister], None).expect("request");
        assert!(after.review_registrations);
        assert_eq!(after.review_mode, ReviewMode::AfterAll);
        s.review_mode = ReviewMode::Off;
        let off = s.run_request(&[Stage::Coregister], None).expect("request");
        assert!(!off.review_registrations);
    }

    #[test]
    fn vat_status_names_the_saved_stimulations() {
        let mut r = full_readiness();
        let s = StageSettings::default();
        r.has_vat_preview = false;
        r.has_norm_nifti = true;
        let (_, plain) = stage_status_detail(Stage::Vat, &s, &r);
        assert_eq!(
            plain,
            "Uses the saved stimulation settings; skipped when there are none"
        );
        r.stimulation_labels = vec!["clinical".to_owned()];
        assert_eq!(
            stage_status_detail(Stage::Vat, &s, &r).1,
            "Computes the VTA of 1 saved stimulation"
        );
        r.stimulation_labels.push("demo".to_owned());
        assert_eq!(
            stage_status_detail(Stage::Vat, &s, &r).1,
            "Computes the VTA of 2 saved stimulations"
        );
        r.has_vat_preview = true;
        assert_eq!(
            stage_status_detail(Stage::Vat, &s, &r).1,
            "Output already present"
        );
    }

    #[test]
    fn stage_status_follows_the_registration_engine() {
        let mut r = full_readiness();
        let s = StageSettings::default();
        assert_eq!(stage_status(Stage::Normalize, &s, &r).0, StageState::Ready);
        r.has_ants_registration = false;
        r.gpu_available = Some(true);
        assert_eq!(stage_status(Stage::Normalize, &s, &r).0, StageState::Ready);
        r.gpu_available = Some(false);
        assert_eq!(
            stage_status(Stage::Normalize, &s, &r).0,
            StageState::Blocked
        );
    }

    fn mr_only_readiness() -> PipelineReadiness {
        PipelineReadiness {
            has_raw_postop_ct: false,
            has_raw_postop_mr: true,
            ..full_readiness()
        }
    }

    #[test]
    fn a_post_operative_mri_alone_can_run_the_pipeline() {
        let s = StageSettings {
            el_model: Some("any".to_owned()),
            ..StageSettings::default()
        };
        let r = mr_only_readiness();
        assert!(s.unreachable_stages(&r).is_empty());
        assert_eq!(
            stage_status(Stage::Coregister, &s, &r),
            (StageState::Ready, "Post-operative MRI found")
        );
        assert_eq!(
            stage_status(Stage::Reconstruct, &s, &r).1,
            "Will use the output of Normalize in this run"
        );
        let normalized = PipelineReadiness {
            has_coreg_warped: true,
            has_norm_nifti: true,
            has_norm_postop_mr: true,
            ..r
        };
        let only = s.only(Stage::Reconstruct);
        assert_eq!(
            stage_status(Stage::Reconstruct, &only, &normalized),
            (
                StageState::Ready,
                "Normalized post-operative image available"
            )
        );
        assert!(only.unreachable_stages(&normalized).is_empty());
    }

    #[test]
    fn normalize_with_a_post_operative_image_needs_the_coregistration() {
        let r = full_readiness();
        let only = StageSettings::default().only(Stage::Normalize);
        assert_eq!(only.unreachable_stages(&r), vec![Stage::Normalize]);
        assert_eq!(
            stage_status(Stage::Normalize, &only, &r).0,
            StageState::NeedsUpstream
        );
        let blockers = only.selection_blockers(&r, &[Stage::Normalize]);
        assert!(
            blockers.iter().any(|b| b.contains("tick Coregister")),
            "{blockers:?}"
        );
        let mut with_coreg = only.clone();
        with_coreg.set_selected(Stage::Coregister, true);
        assert!(with_coreg.unreachable_stages(&r).is_empty());
        assert_eq!(
            stage_status(Stage::Normalize, &with_coreg, &r).1,
            "Will use the output of Coregister in this run"
        );
        let coregistered = PipelineReadiness {
            has_coreg_warped: true,
            ..full_readiness()
        };
        assert!(only.unreachable_stages(&coregistered).is_empty());
        let preop_only = PipelineReadiness {
            has_raw_postop_ct: false,
            ..full_readiness()
        };
        assert!(only.unreachable_stages(&preop_only).is_empty());
    }

    #[test]
    fn coregistration_needs_the_template_for_the_pre_alignment() {
        let s = StageSettings::default().only(Stage::Coregister);
        let r = PipelineReadiness {
            template_exists: false,
            ..full_readiness()
        };
        assert_eq!(s.unreachable_stages(&r), vec![Stage::Coregister]);
        assert_eq!(
            stage_status(Stage::Coregister, &s, &r).0,
            StageState::Blocked
        );
        let r = PipelineReadiness {
            has_ants_registration: false,
            gpu_available: Some(true),
            ..full_readiness()
        };
        let ants_pre = StageSettings {
            preprocess_engine: RegistrationEngine::Ants,
            ..s.clone()
        };
        let (state, why) = stage_status(Stage::Coregister, &ants_pre, &r);
        assert_eq!(state, StageState::Blocked);
        assert!(why.contains("pre-alignment"), "{why}");
        assert_eq!(stage_status(Stage::Coregister, &s, &r).0, StageState::Ready);
    }

    #[test]
    fn precoreg_joins_a_run_that_coregisters_or_normalizes_without_a_post_operative_image() {
        let ct = full_readiness();
        let preop_only = PipelineReadiness {
            has_raw_postop_ct: false,
            ..full_readiness()
        };
        assert_eq!(
            stages_with_precoreg(&[Stage::Coregister, Stage::Normalize], &ct),
            vec![Stage::Precoreg, Stage::Coregister, Stage::Normalize]
        );
        assert_eq!(
            stages_with_precoreg(&[Stage::Preprocess, Stage::Coregister], &ct),
            vec![Stage::Preprocess, Stage::Precoreg, Stage::Coregister]
        );
        assert_eq!(
            stages_with_precoreg(&[Stage::Normalize], &ct),
            vec![Stage::Normalize]
        );
        assert_eq!(
            stages_with_precoreg(&[Stage::Normalize], &preop_only),
            vec![Stage::Precoreg, Stage::Normalize]
        );
        assert_eq!(
            stages_with_precoreg(&[Stage::Reconstruct], &ct),
            vec![Stage::Reconstruct]
        );
        assert_eq!(
            stages_with_precoreg(&[Stage::Precoreg, Stage::Coregister], &ct),
            vec![Stage::Precoreg, Stage::Coregister]
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
    fn default_settings_round_trip_through_json() {
        let s = StageSettings::default();
        let json = serde_json::to_string(&s).expect("serialize");
        let back: StageSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            serde_json::to_value(&s).expect("value"),
            serde_json::to_value(&back).expect("value")
        );
    }

    #[test]
    fn the_atlases_directory_is_saved_and_reaches_the_run_request() {
        let old: StageSettings =
            serde_json::from_str(r#"{"stop_on_error": true}"#).expect("old settings load");
        assert_eq!(old.leaddbs_atlases_dir, None);
        let s = StageSettings {
            leaddbs_atlases_dir: Some(PathBuf::from("/opt/leaddbs")),
            ..StageSettings::default()
        };
        let back: StageSettings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serialize"))
                .expect("deserialize");
        assert_eq!(back.leaddbs_atlases_dir, s.leaddbs_atlases_dir);
        let req = s
            .run_request(&[Stage::Visualize], None)
            .expect("a non-empty stage list builds a request");
        assert_eq!(req.leaddbs_atlases_dir, Some(PathBuf::from("/opt/leaddbs")));
        let req = StageSettings::default()
            .run_request(&[Stage::Visualize], None)
            .expect("request");
        assert_eq!(req.leaddbs_atlases_dir, None);
    }

    #[test]
    fn the_connectomes_directory_is_saved_and_old_settings_load_without_it() {
        let old: StageSettings =
            serde_json::from_str(r#"{"stop_on_error": true}"#).expect("old settings load");
        assert_eq!(old.connectomes_dir, None);
        let s = StageSettings {
            connectomes_dir: Some(PathBuf::from("/data/connectomes")),
            ..StageSettings::default()
        };
        let back: StageSettings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serialize"))
                .expect("deserialize");
        assert_eq!(back.connectomes_dir, s.connectomes_dir);
    }

    #[test]
    fn the_lead_dbs_connectomes_folder_is_saved_and_old_settings_load_without_it() {
        let old: StageSettings =
            serde_json::from_str(r#"{"stop_on_error": true}"#).expect("old settings load");
        assert_eq!(old.leaddbs_connectomes_dir, None);
        let s = StageSettings {
            leaddbs_connectomes_dir: Some(PathBuf::from("/opt/leaddbs/connectomes")),
            ..StageSettings::default()
        };
        let back: StageSettings =
            serde_json::from_str(&serde_json::to_string(&s).expect("serialize"))
                .expect("deserialize");
        assert_eq!(back.leaddbs_connectomes_dir, s.leaddbs_connectomes_dir);
    }

    #[test]
    fn legacy_single_engine_seeds_all_three_steps() {
        let mut s: StageSettings = serde_json::from_str(r#"{"registration_engine": "Ants"}"#)
            .expect("old persisted json loads");
        s.migrate_legacy_engine();
        assert_eq!(s.preprocess_engine, RegistrationEngine::Ants);
        assert_eq!(s.coregister_engine, RegistrationEngine::Ants);
        assert_eq!(s.normalize_engine, RegistrationEngine::Ants);
        let text = serde_json::to_string(&s).expect("serialize");
        assert!(!text.contains("registration_engine"));
        let mut back: StageSettings = serde_json::from_str(&text).expect("reload");
        back.migrate_legacy_engine();
        assert_eq!(back.coregister_engine, RegistrationEngine::Ants);
        let mut empty: StageSettings = serde_json::from_str("{}").expect("empty object loads");
        empty.migrate_legacy_engine();
        assert_eq!(empty.preprocess_engine, RegistrationEngine::Auto);
    }

    #[test]
    fn run_request_carries_each_steps_engine() {
        let s = StageSettings {
            preprocess_engine: RegistrationEngine::Ants,
            coregister_engine: RegistrationEngine::Gpu,
            ..StageSettings::default()
        };
        let req = s.run_request(&[Stage::Normalize], None).expect("request");
        assert_eq!(req.reg_backend_preprocess, Some(RegBackend::Ants));
        assert_eq!(req.reg_backend_coregister, Some(RegBackend::Wgpu));
        assert_eq!(req.reg_backend_normalize, Some(RegBackend::Auto));
    }

    #[test]
    fn legacy_implicit_defaults_load_as_the_template_resolution() {
        for old in ["\"Auto\"", "\"Mm1p0\"", "\"Template\""] {
            let r: NormResolution = serde_json::from_str(old).expect("alias");
            assert_eq!(r, NormResolution::Template, "{old}");
        }
        let s: StageSettings = serde_json::from_str(r#"{"norm_resolution": "Mm1p0"}"#)
            .expect("old persisted json loads");
        assert_eq!(s.norm_resolution, NormResolution::Template);
        let empty: StageSettings = serde_json::from_str("{}").expect("empty loads");
        assert_eq!(empty.norm_resolution, NormResolution::Template);
    }

    #[test]
    fn an_explicit_one_millimetre_choice_survives_a_reload() {
        let s = StageSettings {
            norm_resolution: NormResolution::Mm1p0,
            ..StageSettings::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains("\"Mm1p0Chosen\""), "{json}");
        let back: StageSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.norm_resolution, NormResolution::Mm1p0);
        assert_eq!(back.norm_grid(), NormGrid::Mm(1.0));
        for v in [
            NormResolution::Template,
            NormResolution::Mm0p8,
            NormResolution::Mm0p5,
            NormResolution::FullTemplate,
            NormResolution::Custom,
        ] {
            let json = serde_json::to_string(&v).expect("serialize");
            let back: NormResolution = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, v);
        }
    }

    #[test]
    fn custom_resolution_round_trips_and_resolves() {
        let s = StageSettings {
            norm_resolution: NormResolution::Custom,
            norm_custom_mm: 1.234,
            ..StageSettings::default()
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: StageSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.norm_resolution, NormResolution::Custom);
        assert_eq!(back.norm_custom_mm, 1.234);
        assert_eq!(back.norm_grid(), NormGrid::Mm(1.23));
    }

    #[test]
    fn old_json_without_custom_value_defaults_to_one_and_a_half() {
        let s: StageSettings =
            serde_json::from_str(r#"{"norm_resolution": "Mm0p8"}"#).expect("old json loads");
        assert_eq!(s.norm_custom_mm, 1.5);
        let empty: StageSettings = serde_json::from_str("{}").expect("empty loads");
        assert_eq!(empty.norm_custom_mm, 1.5);
    }

    #[test]
    fn isotropic_mm_resolves_for_every_variant() {
        let mut s = StageSettings::default();
        let cases = [
            (NormResolution::Template, NormGrid::Template),
            (
                NormResolution::Mm1p0,
                NormGrid::Mm(DEFAULT_NORM_ISOTROPIC_MM),
            ),
            (NormResolution::Mm0p8, NormGrid::Mm(0.8)),
            (NormResolution::Mm0p5, NormGrid::Mm(0.5)),
            (NormResolution::FullTemplate, NormGrid::Full),
            (NormResolution::Custom, NormGrid::Mm(1.5)),
        ];
        for (variant, expected) in cases {
            s.norm_resolution = variant;
            assert_eq!(s.norm_grid(), expected, "{variant:?}");
        }
        s.norm_resolution = NormResolution::Custom;
        s.norm_custom_mm = f64::NAN;
        assert_eq!(s.norm_grid(), NormGrid::Mm(1.5));
    }

    #[test]
    fn mm_text_trims_to_two_decimals() {
        assert_eq!(mm_text(1.0), "1.0");
        assert_eq!(mm_text(0.5), "0.5");
        assert_eq!(mm_text(0.75), "0.75");
        assert_eq!(mm_text(0.7500001), "0.75");
    }

    #[test]
    fn template_and_full_labels_show_the_template_voxel_size() {
        assert_eq!(
            NormResolution::Template.label_for(Some(0.5)),
            "Template resolution (0.5 mm, recommended)"
        );
        assert_eq!(
            NormResolution::Template.label_for(None),
            "Template resolution (recommended)"
        );
        assert_eq!(
            NormResolution::FullTemplate.label_for(Some(1.0)),
            "Full template grid (1.0 mm, no resampling)"
        );
        assert_eq!(
            NormResolution::FullTemplate.label_for(None),
            NormResolution::FullTemplate.label()
        );
        assert_eq!(NormResolution::Mm0p8.label_for(Some(1.0)), "0.8 mm");
        assert_eq!(NormResolution::Mm1p0.label(), "1.0 mm");
        assert!(NormResolution::Template
            .hint()
            .contains("steps to the finest coarser grid"));
    }

    #[test]
    fn presets_finer_than_the_template_are_flagged() {
        let flags = |v| {
            [
                NormResolution::Template,
                NormResolution::Mm1p0,
                NormResolution::Mm0p8,
                NormResolution::Mm0p5,
                NormResolution::FullTemplate,
                NormResolution::Custom,
            ]
            .map(|r| r.finer_than(v))
        };
        assert_eq!(flags(Some(1.0)), [false, false, true, true, false, false]);
        assert_eq!(flags(Some(0.5)), [false; 6]);
        assert_eq!(flags(Some(2.0)), [false, true, true, true, false, false]);
        assert_eq!(flags(None), [false; 6]);
    }

    #[test]
    fn custom_range_follows_the_template_voxel() {
        assert_eq!(norm_custom_range(None), (0.3, 3.0));
        assert_eq!(norm_custom_range(Some(0.5)), (0.5, 3.0));
        assert_eq!(norm_custom_range(Some(1.0)), (1.0, 3.0));
        assert_eq!(norm_custom_range(Some(4.0)), (4.0, 4.0));
        assert_eq!(norm_custom_range(Some(0.2)), (0.3, 3.0));
    }

    #[test]
    fn finer_predicate_and_texts() {
        assert!(finer_than_voxel(0.5, 1.0));
        assert!(!finer_than_voxel(1.0, 1.0));
        assert!(!finer_than_voxel(1.0 - 1e-9, 1.0));
        assert!(!finer_than_voxel(1.0, 1.004));
        assert!(finer_than_voxel(0.5, 1.004));
        assert!(finer_selection_warning(1.0).starts_with("Finer than the template's 1.0 mm voxels"));
        assert!(finer_disabled_hover(0.75).contains("0.75 mm"));
        assert_eq!(
            grid_size_text([197, 233, 189]),
            "Grid: 197 × 233 × 189 voxels (8.7 million)"
        );
    }

    #[test]
    fn saved_thread_names_still_load() {
        let load = |v: &str| -> Option<u32> {
            let json = format!("{{\"ants_threads\": {v}}}");
            serde_json::from_str::<StageSettings>(&json)
                .expect("settings load")
                .ants_threads
        };
        assert_eq!(load("\"Auto\""), None);
        assert_eq!(load("\"One\""), Some(1));
        assert_eq!(load("\"Two\""), Some(2));
        assert_eq!(load("\"Four\""), Some(4));
        assert_eq!(load("\"Eight\""), Some(8));
        assert_eq!(load("6"), Some(6));
        assert_eq!(load("null"), None);
        assert_eq!(StageSettings::default().ants_threads, None);
    }

    #[test]
    fn native_ct_reconstruction_has_its_own_cli_value() {
        assert_eq!(ReconMethod::TracCoreNative.cli_value(), "trac_core_native");
    }

    #[test]
    fn engine_hints_carry_no_run_of_spaces() {
        for e in [
            RegistrationEngine::Auto,
            RegistrationEngine::Gpu,
            RegistrationEngine::Ants,
        ] {
            assert!(!e.hint().contains("  "), "{}", e.hint());
        }
    }

    fn reviewed_readiness(
        coreg: RegistrationReview,
        norm: RegistrationReview,
    ) -> PipelineReadiness {
        PipelineReadiness {
            has_coreg_warped: true,
            has_norm_nifti: true,
            coreg_review: coreg,
            norm_review: norm,
            ..full_readiness()
        }
    }

    #[test]
    fn a_rejected_upstream_registration_warns_on_its_dependents() {
        let s = StageSettings::default().only(Stage::Reconstruct);
        let r = reviewed_readiness(RegistrationReview::Rejected, RegistrationReview::Approved);
        let w = s.upstream_review_warnings(&r, Stage::Reconstruct);
        assert_eq!(
            w,
            vec![UpstreamReviewWarning {
                upstream: Stage::Coregister,
                state: RegistrationReview::Rejected
            }]
        );
        assert_eq!(w[0].text(), "Uses a coregistration that was rejected");
    }

    #[test]
    fn an_unreviewed_normalization_warns_on_reconstruct_and_later_steps() {
        let r = reviewed_readiness(
            RegistrationReview::Approved,
            RegistrationReview::NotReviewed,
        );
        for stage in [
            Stage::Reconstruct,
            Stage::Orientation,
            Stage::Vat,
            Stage::Visualize,
        ] {
            let s = StageSettings::default().only(stage);
            let w = s.upstream_review_warnings(&r, stage);
            assert_eq!(w.len(), 1, "{stage:?}");
            assert_eq!(w[0].upstream, Stage::Normalize);
            assert_eq!(w[0].text(), "Uses a normalization that was not reviewed");
        }
    }

    #[test]
    fn normalize_depends_on_the_coregistration_review() {
        let r = reviewed_readiness(
            RegistrationReview::NotReviewed,
            RegistrationReview::Approved,
        );
        let s = StageSettings::default().only(Stage::Normalize);
        let w = s.upstream_review_warnings(&r, Stage::Normalize);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].text(), "Uses a coregistration that was not reviewed");
        assert!(s.upstream_review_warnings(&r, Stage::Coregister).is_empty());
    }

    #[test]
    fn an_approved_registration_gives_no_warning() {
        let r = reviewed_readiness(RegistrationReview::Approved, RegistrationReview::Approved);
        let s = StageSettings::default();
        for stage in Stage::work_sequence() {
            assert!(s.upstream_review_warnings(&r, *stage).is_empty());
        }
    }

    #[test]
    fn no_warning_when_the_upstream_step_runs_in_the_same_run() {
        let r = reviewed_readiness(RegistrationReview::Rejected, RegistrationReview::Rejected);
        let s = StageSettings {
            overwrite_results: true,
            ..StageSettings::default()
        };
        for stage in Stage::work_sequence() {
            assert!(
                s.upstream_review_warnings(&r, *stage).is_empty(),
                "{stage:?}"
            );
        }
        let mut only_recon = StageSettings::default().only(Stage::Reconstruct);
        only_recon.overwrite_results = true;
        only_recon.set_selected(Stage::Normalize, true);
        let w = only_recon.upstream_review_warnings(&r, Stage::Reconstruct);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].upstream, Stage::Coregister);
    }

    #[test]
    fn no_warning_without_output_or_when_the_dependent_is_unticked() {
        let mut r = reviewed_readiness(RegistrationReview::Rejected, RegistrationReview::Rejected);
        let s = StageSettings::default().only(Stage::Reconstruct);
        assert!(s
            .upstream_review_warnings(&r, Stage::Orientation)
            .is_empty());
        r.has_coreg_warped = false;
        r.has_norm_nifti = false;
        assert!(s
            .upstream_review_warnings(&r, Stage::Reconstruct)
            .is_empty());
    }

    #[test]
    fn the_review_setting_does_not_change_the_warnings() {
        let r = reviewed_readiness(
            RegistrationReview::Rejected,
            RegistrationReview::NotReviewed,
        );
        let mut s = StageSettings::default().only(Stage::Reconstruct);
        let on = s.upstream_review_warnings(&r, Stage::Reconstruct);
        s.review_mode = ReviewMode::Off;
        assert_eq!(on, s.upstream_review_warnings(&r, Stage::Reconstruct));
        assert_eq!(on.len(), 2);
    }

    #[test]
    fn warnings_never_become_blockers() {
        let mut r = reviewed_readiness(RegistrationReview::Rejected, RegistrationReview::Rejected);
        r.has_norm_postop_ct = true;
        let mut s = StageSettings::default().only(Stage::Reconstruct);
        s.el_model = Some("any".to_owned());
        s.overwrite_results = true;
        let runnable = s.selected_runnable_stages(&r);
        assert!(s.selection_blockers(&r, &runnable).is_empty());
    }
}
