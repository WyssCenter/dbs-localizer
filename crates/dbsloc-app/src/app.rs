// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Root application state and [`eframe::App`] implementation.
//!
//! The frame loop here is deliberately allocation- and I/O-free: directory
//! scans, manifest loading and the pipeline itself all run on worker threads
//! that request a repaint when they have something new. Nothing polls.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use dbsloc_pipeline::batch::{ReviewPolicy, SubjectTally};
use dbsloc_pipeline::{
    protocol::RunEvent, run_pipeline_channel, run_pipeline_channel_with_review, stages::Stage,
    PngWriter, ReviewSetup,
};
use dbsloc_ui::{BatchPhase, BatchQueue, QueuedRun, ReviewMode, SubjectChecks, SubjectResult};
use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use serde::{Deserialize, Serialize};

use crate::guided::{
    guided_can_run, guided_refusal, guided_run_action, guided_step_action, guided_step_rerun,
    GuidedInputs, GuidedNav, GuidedStep,
};
use crate::import_wizard::{ImportWizard, ImportWizardAction};
use crate::landing::{LandingAction, LandingView};
use crate::panels::{
    directory_row, em, palette, reveal_in_file_manager, stages_with_precoreg, BatchInfo,
    DirectoryEvent, DirectoryRow, LogPanel, OutputDetail, PipelineReadiness, ReadinessCache,
    RefineActivity, RunRequest, ScenePanel, ScenePanelAction, SceneSettings, StagePanel,
    StagePanelAction, StageSettings, SubjectPanel, OUTPUT_DIRECTORY_HOVER,
};
use crate::refine::{pipeline_edits, report_lines, RefineOwner, RefineTab, RefineTabOutput};
use crate::review::{BridgeMsg, ReviewOutput, ReviewOwner, ReviewPanel};

/// Colour tokens, shared with the embedded scene viewer so one window never
/// mixes two palettes.
pub use dbsloc_viz::ui_tokens as tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TabKind {
    Subjects,
    Pipeline,
    Output,
    Scene,
    Review,
    Refine,
    Stimulation,
    Group,
}

impl TabKind {
    pub const ALL: [TabKind; 8] = [
        TabKind::Subjects,
        TabKind::Pipeline,
        TabKind::Output,
        TabKind::Scene,
        TabKind::Review,
        TabKind::Refine,
        TabKind::Stimulation,
        TabKind::Group,
    ];

    fn title(self) -> &'static str {
        match self {
            TabKind::Subjects => "Subjects",
            TabKind::Pipeline => "Pipeline",
            TabKind::Output => "Output",
            TabKind::Scene => "3D Viewer",
            TabKind::Review => "Registration review",
            TabKind::Refine => "Refine localization",
            TabKind::Stimulation => "Stimulation",
            TabKind::Group => "Group analysis",
        }
    }

    pub fn is_transient(self) -> bool {
        matches!(
            self,
            TabKind::Review | TabKind::Refine | TabKind::Stimulation | TabKind::Group
        )
    }

    fn hint(self) -> &'static str {
        match self {
            TabKind::Subjects => "Pick the BIDS subject to work on",
            TabKind::Pipeline => "Choose the steps to run and their options",
            TabKind::Output => "Streamed output of the running pipeline",
            TabKind::Scene => "Interactive 3D scene of the reconstruction",
            TabKind::Review => "Approve or reject a coregistration or normalization result",
            TabKind::Refine => "Correct the placement of a reconstructed lead on the post-op image",
            TabKind::Stimulation => {
                "Edit the stimulation settings and preview the volume of activated tissue"
            }
            TabKind::Group => {
                "Cohort of patients: groups, clinical variables, the group stimulation and the group 3D view"
            }
        }
    }
}

fn default_dock_state() -> DockState<TabKind> {
    let mut dock = DockState::new(vec![TabKind::Output, TabKind::Scene]);
    let surface = dock.main_surface_mut();
    let [_, left] = surface.split_left(NodeIndex::root(), 0.28, vec![TabKind::Subjects]);
    surface.split_below(left, 0.24, vec![TabKind::Pipeline]);
    dock
}

fn ensure_tab_beside_scene(dock: &mut DockState<TabKind>, tab: TabKind) {
    if dock.find_tab(&tab).is_some() {
        return;
    }
    match dock.find_tab(&TabKind::Scene) {
        Some(path) => {
            dock.set_focused_node_and_surface(path.node_path());
            dock.push_to_focused_leaf(tab);
            if let Some(scene) = dock.find_tab(&TabKind::Scene) {
                let _ = dock.set_active_tab(scene);
            }
        }
        None => dock.push_to_focused_leaf(tab),
    }
}

fn focus_in_dock(dock: &mut DockState<TabKind>, tab: TabKind) {
    if tab.is_transient() {
        ensure_tab_beside_scene(dock, tab);
    }
    if let Some(path) = dock.find_tab(&tab) {
        let _ = dock.set_active_tab(path);
    } else {
        dock.push_to_focused_leaf(tab);
    }
}

fn remove_transient_tabs(dock: &mut DockState<TabKind>) {
    for tab in TabKind::ALL.into_iter().filter(|t| t.is_transient()) {
        while let Some(path) = dock.find_tab(&tab) {
            dock.remove_tab(path);
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    fn label(self) -> &'static str {
        match self {
            Self::System => "Follow system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageRunStatus {
    Pending,
    Running,
    Ok,
    /// The step itself decided to do nothing — `RunEvent::StageSkipped`.
    Skipped,
    /// The run was stopped before this step could start.
    Cancelled,
    Failed,
    AwaitingReview,
    Rejected,
    /// The registration gate stopped the subject until a later review (runs of several
    /// subjects with the review after all subjects).
    Deferred,
}

pub(crate) fn run_summary(ok: u32, failed: u32, skipped: u32, rework: u32) -> String {
    if rework == 0 {
        return format!("{ok} completed, {failed} failed, {skipped} skipped");
    }
    if ok == 0 && failed == 0 && skipped == 0 {
        return format!("{rework} needs rework");
    }
    format!("{ok} completed, {failed} failed, {skipped} skipped, {rework} needs rework")
}

fn format_seconds(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    if total < 60 {
        format!("{total} s")
    } else {
        format!("{} min {:02} s", total / 60, total % 60)
    }
}

fn skip_reason_text(stage_label: &str, raw: &str) -> String {
    let raw = raw.trim();
    let raw = raw.strip_prefix("skip:").unwrap_or(raw).trim();
    let stripped = raw
        .get(..stage_label.len())
        .filter(|head| head.eq_ignore_ascii_case(stage_label))
        .and_then(|_| raw.get(stage_label.len()..))
        .and_then(|rest| rest.strip_prefix(':'));
    let raw = stripped.map_or(raw, str::trim);
    if raw == "approved" {
        return "its output was approved earlier".to_owned();
    }
    raw.to_owned()
}

#[derive(Default)]
pub struct RunProgress {
    planned: Vec<Stage>,
    status: Vec<StageRunStatus>,
    skip_reasons: Vec<Option<String>>,
    started: Option<Instant>,
    final_elapsed: Option<Duration>,
    summary: Option<String>,
    defer: bool,
}

impl RunProgress {
    #[cfg(test)]
    pub(crate) fn start(&mut self, stages: &[Stage]) {
        self.start_with(stages, false);
    }

    /// `defer`: an unapproved registration gate means "awaiting review", not "rejected".
    pub(crate) fn start_with(&mut self, stages: &[Stage], defer: bool) {
        self.defer = defer;
        self.planned = stages.to_vec();
        self.status = vec![StageRunStatus::Pending; stages.len()];
        self.skip_reasons = vec![None; stages.len()];
        self.started = Some(Instant::now());
        self.final_elapsed = None;
        self.summary = None;
    }

    fn set(&mut self, stage_label: &str, status: StageRunStatus) {
        if let Some(i) = self.planned.iter().position(|s| s.label() == stage_label) {
            self.status[i] = status;
        }
    }

    fn status_of_label(&self, stage_label: &str) -> Option<StageRunStatus> {
        self.planned
            .iter()
            .position(|s| s.label() == stage_label)
            .map(|i| self.status[i])
    }

    pub(crate) fn on_review_requested(&mut self, stage: Stage) {
        self.set(stage.label(), StageRunStatus::AwaitingReview);
    }

    pub(crate) fn on_review_answered(&mut self, stage: Stage) {
        if self.status_of_label(stage.label()) == Some(StageRunStatus::AwaitingReview) {
            self.set(stage.label(), StageRunStatus::Running);
        }
    }

    pub(crate) fn is_rejected_label(&self, stage_label: &str) -> bool {
        matches!(
            self.status_of_label(stage_label),
            Some(StageRunStatus::Rejected | StageRunStatus::Deferred)
        )
    }

    fn count(&self, want: StageRunStatus) -> u32 {
        u32::try_from(self.status.iter().filter(|s| **s == want).count()).unwrap_or(u32::MAX)
    }

    pub(crate) fn on_event(&mut self, ev: &RunEvent) {
        match ev {
            RunEvent::StageStarted { stage } => {
                if self.status_of_label(stage) != Some(StageRunStatus::AwaitingReview) {
                    self.set(stage, StageRunStatus::Running);
                }
            }
            RunEvent::StageCompleted { stage, .. } => self.set(stage, StageRunStatus::Ok),
            RunEvent::StageSkipped {
                stage,
                reason,
                result,
            } => {
                self.set(stage, StageRunStatus::Skipped);
                if let Some(i) = self.planned.iter().position(|s| s.label() == stage) {
                    self.skip_reasons[i] =
                        Some(skip_reason_text(stage, result.as_deref().unwrap_or(reason)));
                }
            }
            RunEvent::StageFailed { stage, .. } => {
                if !self.is_rejected_label(stage) {
                    self.set(stage, StageRunStatus::Failed);
                }
            }
            RunEvent::StageAwaitingQc { stage } => self.set(
                stage,
                if self.defer {
                    StageRunStatus::Deferred
                } else {
                    StageRunStatus::Rejected
                },
            ),
            RunEvent::PipelineCompleted {
                stages_ok,
                stages_failed,
                stages_skipped,
            } => {
                self.final_elapsed = self.started.map(|t| t.elapsed());
                let rework = self.count(StageRunStatus::Rejected);
                let deferred = self.count(StageRunStatus::Deferred);
                let mut summary = run_summary(
                    *stages_ok,
                    stages_failed.saturating_sub(rework + deferred),
                    *stages_skipped,
                    rework,
                );
                if deferred > 0 {
                    summary.push_str(&format!(", {deferred} awaiting review"));
                }
                self.summary = Some(summary);
                for s in &mut self.status {
                    if matches!(
                        *s,
                        StageRunStatus::Pending
                            | StageRunStatus::Running
                            | StageRunStatus::AwaitingReview
                    ) {
                        *s = StageRunStatus::Cancelled;
                    }
                }
            }
            RunEvent::LogLine { .. } => {}
        }
    }

    pub(crate) fn abandon(&mut self) {
        if self.planned.is_empty() || self.summary.is_some() {
            return;
        }
        self.final_elapsed = self.started.map(|t| t.elapsed());
        for s in &mut self.status {
            *s = match *s {
                StageRunStatus::Running | StageRunStatus::AwaitingReview => StageRunStatus::Failed,
                StageRunStatus::Pending => StageRunStatus::Cancelled,
                other => other,
            };
        }
        let count = |want: StageRunStatus| {
            u32::try_from(self.status.iter().filter(|s| **s == want).count()).unwrap_or(u32::MAX)
        };
        self.summary = Some(run_summary(
            count(StageRunStatus::Ok),
            count(StageRunStatus::Failed),
            count(StageRunStatus::Skipped),
            count(StageRunStatus::Rejected),
        ));
    }

    pub fn status_of(&self, stage: Stage) -> Option<StageRunStatus> {
        self.planned
            .iter()
            .position(|s| *s == stage)
            .map(|i| self.status[i])
    }

    pub fn skip_reason(&self, stage: Stage) -> Option<&str> {
        let i = self.planned.iter().position(|s| *s == stage)?;
        if self.status[i] != StageRunStatus::Skipped {
            return None;
        }
        self.skip_reasons[i].as_deref().filter(|r| !r.is_empty())
    }

    pub fn has_result(&self) -> bool {
        !self.planned.is_empty()
    }

    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    pub fn any_failed(&self) -> bool {
        self.status.contains(&StageRunStatus::Failed)
    }

    pub fn any_cancelled(&self) -> bool {
        self.status.contains(&StageRunStatus::Cancelled)
    }

    pub fn any_rejected(&self) -> bool {
        self.status.contains(&StageRunStatus::Rejected)
            || self.status.contains(&StageRunStatus::Deferred)
    }

    fn done_count(&self) -> usize {
        self.status
            .iter()
            .filter(|s| {
                !matches!(
                    s,
                    StageRunStatus::Pending
                        | StageRunStatus::Running
                        | StageRunStatus::AwaitingReview
                )
            })
            .count()
    }

    pub fn fraction(&self) -> f32 {
        if self.planned.is_empty() {
            return 0.0;
        }
        self.done_count() as f32 / self.planned.len() as f32
    }

    pub fn elapsed(&self) -> Duration {
        self.final_elapsed
            .or_else(|| self.started.map(|t| t.elapsed()))
            .unwrap_or_default()
    }

    /// Elapsed time, worded so a sub-second run does not read as `00:00`.
    fn clock(&self) -> String {
        let d = self.elapsed();
        if d < Duration::from_secs(1) {
            return format!("{} ms", d.as_millis());
        }
        let secs = d.as_secs();
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }

    pub fn progress_text(&self) -> String {
        let clock = self.clock();
        let running = self
            .planned
            .iter()
            .zip(&self.status)
            .find(|(_, s)| **s == StageRunStatus::Running)
            .map(|(stage, _)| stage.label());
        if let Some(stage) = running {
            return format!(
                "{}/{} · {stage} · {clock}",
                self.done_count(),
                self.planned.len()
            );
        }
        let awaiting = self
            .planned
            .iter()
            .zip(&self.status)
            .find(|(_, s)| **s == StageRunStatus::AwaitingReview)
            .map(|(stage, _)| stage.label());
        if let Some(stage) = awaiting {
            return format!(
                "{}/{} · {stage} · awaiting review · {clock}",
                self.done_count(),
                self.planned.len()
            );
        }
        let failed = self
            .status
            .iter()
            .filter(|s| **s == StageRunStatus::Failed)
            .count();
        let cancelled = self
            .status
            .iter()
            .filter(|s| **s == StageRunStatus::Cancelled)
            .count();
        let rejected = self
            .status
            .iter()
            .filter(|s| **s == StageRunStatus::Rejected)
            .count();
        let deferred = self.count(StageRunStatus::Deferred);
        let outcome = if failed > 0 {
            format!(" · {failed} failed")
        } else if rejected > 0 {
            format!(" · {rejected} needs rework")
        } else if deferred > 0 {
            format!(" · {deferred} awaiting review")
        } else if cancelled > 0 {
            format!(" · {cancelled} cancelled")
        } else {
            String::new()
        };
        format!(
            "{}/{} steps{outcome} · {clock}",
            self.done_count(),
            self.planned.len()
        )
    }
}

/// How many recent datasets and recent subjects the landing screen remembers.
const RECENT_LIMIT: usize = 8;

/// One subject of one dataset, as listed on the landing screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSubject {
    pub bids_dir: String,
    pub subject: String,
}

/// Move `(bids_dir, subject)` to the front of the recent list, without
/// duplicates and capped at [`RECENT_LIMIT`].
pub fn remember_subject_in_state(state: &mut DbslocAppState, bids_dir: &str, subject: &str) {
    let bids_dir = bids_dir.trim();
    if bids_dir.is_empty() || subject.is_empty() {
        return;
    }
    state
        .recent_subjects
        .retain(|r| !(r.bids_dir == bids_dir && r.subject == subject));
    state.recent_subjects.insert(
        0,
        RecentSubject {
            bids_dir: bids_dir.to_owned(),
            subject: subject.to_owned(),
        },
    );
    state.recent_subjects.truncate(RECENT_LIMIT);
}

pub fn remember_output_dir_in_state(state: &mut DbslocAppState, dir: &str) {
    let dir = dir.trim();
    if dir.is_empty() {
        return;
    }
    state.recent_output_dirs.retain(|p| p != dir);
    state.recent_output_dirs.insert(0, dir.to_owned());
    state.recent_output_dirs.truncate(RECENT_LIMIT);
}

/// The output directory chosen for one dataset; a dataset without an entry uses
/// `<dataset>/derivatives/leaddbs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetOutputDir {
    pub bids_dir: String,
    pub output_dir: String,
}

pub fn output_dir_in(list: &[DatasetOutputDir], bids_dir: &str) -> Option<PathBuf> {
    let key = bids_dir.trim();
    list.iter()
        .find(|d| d.bids_dir == key)
        .map(|d| PathBuf::from(d.output_dir.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

struct GuidedVtaLoad {
    label: String,
    key: usize,
    rx: mpsc::Receiver<Vec<dbsloc_viz::vta_mesh::VtaSurface>>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct DbslocAppState {
    pub bids_dir: String,
    pub recent_bids_dirs: Vec<String>,
    pub recent_output_dirs: Vec<String>,
    pub recent_subjects: Vec<RecentSubject>,
    /// Group files opened last, newest first.
    pub recent_groups: Vec<String>,
    /// Connectome last chosen per group analysis kind.
    pub group_connectomes: std::collections::BTreeMap<String, String>,
    pub stim_connectome: Option<String>,
    pub connectome_cache: crate::connectome_cache_ui::CacheMode,
    /// Output directory per dataset (absolute paths), restored when the dataset is chosen again.
    pub output_dirs: Vec<DatasetOutputDir>,
    /// Walk through the steps one at a time after opening a subject.
    pub guided_default: bool,
    pub selected_subject: Option<String>,
    /// Subjects ticked for a run of several subjects (per dataset).
    pub checked_subjects: SubjectChecks,
    pub normalization_template: Option<String>,
    /// Optional T2 template for the multispectral SyN; `None` uses the T1
    /// template's sibling T2 when one exists.
    pub normalization_template_t2: Option<String>,
    pub theme: ThemeChoice,
    pub stage: StageSettings,
    pub scene: SceneSettings,
    pub navigation: dbsloc_viz::NavigationScheme,
    pub output_detail: OutputDetail,
    pub follow_output: bool,
    pub wrap_output: bool,
    /// Serialized `egui_dock` layout, so a rearranged workspace survives restart.
    pub dock: Option<DockState<TabKind>>,
    /// The first-start question about importing Lead-DBS data was asked (`specs/17` §4.3).
    pub leaddbs_import_offered: bool,
    #[serde(skip)]
    pub leaddbs_import_request: bool,
    #[serde(skip)]
    pub output_dir_edit: String,
    #[serde(skip)]
    pub template_ui: crate::template_picker::TemplateUi,
    #[serde(skip)]
    pub norm_grid: crate::panels::TemplateGridCache,
    #[serde(skip)]
    pub atlas_dir_pick: crate::panels::FolderPick,
    #[serde(skip)]
    pub guided_stimulation: Option<String>,
}

impl Default for DbslocAppState {
    fn default() -> Self {
        Self {
            bids_dir: String::new(),
            recent_bids_dirs: Vec::new(),
            recent_output_dirs: Vec::new(),
            recent_subjects: Vec::new(),
            recent_groups: Vec::new(),
            group_connectomes: std::collections::BTreeMap::new(),
            stim_connectome: None,
            connectome_cache: crate::connectome_cache_ui::CacheMode::default(),
            output_dirs: Vec::new(),
            guided_default: true,
            selected_subject: None,
            checked_subjects: SubjectChecks::default(),
            normalization_template: None,
            normalization_template_t2: None,
            theme: ThemeChoice::default(),
            stage: StageSettings::default(),
            scene: SceneSettings::default(),
            navigation: dbsloc_viz::NavigationScheme::default(),
            output_detail: OutputDetail::default(),
            follow_output: true,
            wrap_output: false,
            dock: None,
            leaddbs_import_offered: false,
            leaddbs_import_request: false,
            output_dir_edit: String::new(),
            template_ui: crate::template_picker::TemplateUi::default(),
            norm_grid: crate::panels::TemplateGridCache::default(),
            atlas_dir_pick: crate::panels::FolderPick::default(),
            guided_stimulation: None,
        }
    }
}

impl DbslocAppState {
    /// Output directory chosen for the current dataset; `None` = the default.
    pub fn output_dir(&self) -> Option<PathBuf> {
        output_dir_in(&self.output_dirs, &self.bids_dir)
    }

    pub fn output_dir_for(&self, bids_dir: &Path) -> Option<PathBuf> {
        output_dir_in(&self.output_dirs, &bids_dir.to_string_lossy())
    }

    /// Output root of `bids_dir` without filesystem access, safe on the paint thread.
    pub fn derivatives_root_for(&self, bids_dir: &Path) -> PathBuf {
        dbsloc_pipeline::paths::derivatives_root_lexical(
            bids_dir,
            self.output_dir_for(bids_dir).as_deref(),
        )
    }

    /// Output root of the current dataset; `None` when no dataset is chosen.
    pub fn derivatives_root(&self) -> Option<PathBuf> {
        let bids = self.bids_dir.trim();
        (!bids.is_empty()).then(|| self.derivatives_root_for(Path::new(bids)))
    }

    pub fn subject_derivatives_dir(&self, subject_folder: &str) -> Option<PathBuf> {
        self.derivatives_root().map(|r| r.join(subject_folder))
    }

    /// Choose the output directory of the current dataset; `None` (or the default folder itself)
    /// restores the default.
    pub fn set_output_dir(&mut self, dir: Option<&str>) {
        let key = self.bids_dir.trim().to_owned();
        if key.is_empty() {
            return;
        }
        self.output_dirs.retain(|d| d.bids_dir != key);
        let Some(dir) = dir.map(str::trim).filter(|d| !d.is_empty()) else {
            return;
        };
        let abs = dbsloc_pipeline::paths::absolute(Path::new(dir));
        let default = dbsloc_pipeline::paths::default_derivatives_root(Path::new(&key));
        if abs == dbsloc_pipeline::paths::absolute(&default) {
            return;
        }
        self.output_dirs.push(DatasetOutputDir {
            bids_dir: key,
            output_dir: abs.to_string_lossy().into_owned(),
        });
    }
}

struct ErrorEntry {
    title: String,
    body: String,
}

/// One or more errors that share a body — a single root cause.
struct GroupedError {
    title: String,
    body: String,
    /// Index of the first contributing entry, for dismissal.
    first_index: usize,
}

/// Collapse errors with identical bodies into one entry naming every step.
fn add_error(errors: &mut Vec<ErrorEntry>, title: String, body: String) -> bool {
    if errors.iter().any(|e| e.title == title && e.body == body) {
        return false;
    }
    errors.push(ErrorEntry { title, body });
    true
}

fn group_errors(errors: &[ErrorEntry]) -> Vec<GroupedError> {
    let mut out: Vec<GroupedError> = Vec::new();
    let mut titles: Vec<Vec<String>> = Vec::new();
    for (i, e) in errors.iter().enumerate() {
        match out.iter().position(|g| g.body == e.body) {
            Some(k) => titles[k].push(e.title.clone()),
            None => {
                out.push(GroupedError {
                    title: e.title.clone(),
                    body: e.body.clone(),
                    first_index: i,
                });
                titles.push(vec![e.title.clone()]);
            }
        }
    }
    for (g, t) in out.iter_mut().zip(titles) {
        if t.len() > 1 {
            let shared = common_prefix(&t);
            let names: Vec<&str> = t.iter().map(|x| x[shared.len()..].trim()).collect();
            g.title = format!(
                "{}{} ({} steps, same cause)",
                shared,
                names.join(", "),
                t.len()
            );
        }
    }
    out
}

/// Longest prefix shared by every string, trimmed back to the last `": "`.
fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut len = first.len();
    for other in &items[1..] {
        len = first
            .bytes()
            .zip(other.bytes())
            .take(len)
            .take_while(|(a, b)| a == b)
            .count();
    }
    match first[..len].rfind(": ") {
        Some(i) => first[..i + 2].to_owned(),
        None => String::new(),
    }
}

pub struct DbslocApp {
    pub state: DbslocAppState,
    dock: DockState<TabKind>,
    /// The Wyss avatar, decoded once at creation and tinted per theme when drawn.
    logo: egui::TextureHandle,
    subject_panel: SubjectPanel,
    stage_panel: StagePanel,
    pub log_panel: LogPanel,
    scene_panel: ScenePanel,
    review_panel: ReviewPanel,
    review_rx: Option<mpsc::Receiver<BridgeMsg>>,
    review_prepare: Option<mpsc::Receiver<PreparedReview>>,
    run_owner: Option<ReviewOwner>,
    prepare_owner: Option<ReviewOwner>,
    review_finishing: Arc<AtomicUsize>,
    refine_tab: RefineTab,
    stimulation_tab: crate::stimulation_panel::StimulationTab,
    fibre_files: crate::connectivity::FibreFiles,
    group_tab: crate::group_panel::GroupTab,
    group_look_pending: Option<crate::group_panel::GroupLook>,
    group_layers_pending: Vec<crate::group_explorers::GroupLayer>,
    group_layers_view_started: bool,
    screenshot_group: bool,
    group_scene_since: Option<u32>,
    stimulation_scene: Option<(u64, usize)>,
    screenshot_stimulation: bool,
    stimulation_scene_since: Option<u32>,
    refine_prepare: Option<mpsc::Receiver<PreparedRefine>>,
    refine_save: Option<mpsc::Receiver<RefineSaveMsg>>,
    refine_opts: Option<dbsloc_pipeline::options::RunOptions>,
    refine_prepare_owner: Option<RefineOwner>,
    refine_prepare_follow: bool,
    refine_prepare_epoch: u64,
    job_epoch: u64,
    job_epoch_importing: bool,
    follow_readiness_seen: u64,
    refine_cancel: Arc<AtomicBool>,
    refine_busy: Arc<AtomicUsize>,
    screenshot_refine: bool,
    readiness: ReadinessCache,
    gpu_probe_seen: bool,
    progress: RunProgress,
    _runner_thread: Option<thread::JoinHandle<()>>,
    _relay_thread: Option<thread::JoinHandle<()>>,
    pipeline_cancel: Arc<AtomicBool>,
    pending_bids_pick: Option<mpsc::Receiver<Option<String>>>,
    pending_output_pick: Option<mpsc::Receiver<Option<String>>>,
    pending_template_pick: Option<mpsc::Receiver<Option<String>>>,
    pending_template_t2_pick: Option<mpsc::Receiver<Option<String>>>,
    pending_manifest_pick: Option<mpsc::Receiver<Option<String>>>,
    pending_backdrop_pick: Option<mpsc::Receiver<Option<PathBuf>>>,
    last_saved_state: String,
    last_persist_check: Instant,
    applied_theme: Option<ThemeChoice>,
    open_viewer_after_reconstruct: bool,
    errors: Vec<ErrorEntry>,
    show_shortcuts: bool,
    window_was_focused: bool,
    gpu_status_logged: bool,
    confirm_overwrite: Option<PendingOverwrite>,
    quit_after_run: bool,
    /// Frames drawn so far — only used by the screenshot hook below.
    frames: u32,
    screenshot_requested: bool,
    /// `DBSLOC_APP_SCREENSHOT_PNG`, resolved once at construction.
    screenshot_png: Option<PathBuf>,
    /// `DBSLOC_APP_SCREENSHOT_ACTION=run`: start a run before capturing.
    screenshot_action_run: bool,
    /// `DBSLOC_APP_SCREENSHOT_ACTION=focus`: put keyboard focus on the Run
    /// button before capturing, so the focus indicator can be seen in a still.
    screenshot_action_focus: bool,
    /// `DBSLOC_APP_SCREENSHOT_ACTION=viewer-popout`: open the scene in its own
    /// window first and keep it open for a moment after the capture.
    screenshot_action_popout: bool,
    screenshot_viewer_docked: bool,
    popout_since: Option<u32>,
    screenshot_tab_popout: Option<TabKind>,
    screenshot_popout_pending: bool,
    screenshot_mode: bool,
    screenshot_review: Option<ReviewShot>,
    review_ready_since: Option<u32>,
    screenshot_close_at: Option<Instant>,
    /// Inputs the background scans key on, and when they last changed.
    scan_key: Option<(String, Option<String>, Option<String>)>,
    scan_settled_at: Instant,
    /// A close was requested while a run was in progress.
    confirm_quit: bool,
    quit_waits_for_import: bool,
    quit_cancels_import: bool,
    /// The user explicitly chose "Quit now" from that dialog.
    quit_approved: bool,
    /// The start screen replaces the workspace. Always true at launch.
    show_landing: bool,
    /// The guided run replaces the dock while the start screen is away.
    guided: bool,
    guided_step: GuidedStep,
    guided_vta_want: Option<String>,
    guided_vta_applied: Option<(String, usize)>,
    guided_vta_load: Option<GuidedVtaLoad>,
    landing: LandingView,
    import: Option<ImportWizard>,
    report_import: crate::report_import::ReportImportHost,
    pending_report_pick: Option<mpsc::Receiver<Vec<PathBuf>>>,
    screenshot_report: bool,
    connectome_panel: crate::connectome_panel::ConnectomePanel,
    leaddbs_import: crate::leaddbs_import::LeadDbsImport,
    connectomes_setting_push: crate::connectome_roots::SettingPush,
    screenshot_leaddbs: bool,
    screenshot_consent: Option<crate::connectome_cache_ui::ConsentDialog>,
    image_preview: crate::panels::ImagePreviewHost,
    /// "Open 3D viewer" on the Review step had to write the scene first.
    open_viewer_after_visualize: bool,
    batch: Option<BatchRun>,
    /// The stages a batch subject's worker chose after scanning that subject's files.
    batch_plan_rx: Option<mpsc::Receiver<Vec<Stage>>>,
    /// Per-subject results and the summary line of the last finished batch.
    last_batch: Option<(Vec<SubjectResult>, String)>,
    confirm_batch_stop: bool,
}

/// A run of several subjects (`specs/13` §9–§11): subjects run one after another with the
/// settings captured when the batch started.
struct BatchRun {
    queue: BatchQueue,
    req: RunRequest,
    settings: StageSettings,
    bids_dir: String,
    tally: SubjectTally,
    started: Instant,
}

impl BatchRun {
    fn policy(&self) -> ReviewPolicy {
        review_policy(self.queue.mode())
    }
}

fn review_policy(mode: ReviewMode) -> ReviewPolicy {
    match mode {
        ReviewMode::DuringRun => ReviewPolicy::Interactive,
        ReviewMode::AfterAll => ReviewPolicy::Defer,
        ReviewMode::Off => ReviewPolicy::Off,
    }
}

/// How a run's registration check is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewKind {
    /// The run waits on the in-process review hook.
    Hook,
    /// The gate does not wait; an unapproved registration stops the subject.
    Defer,
    Off,
}

/// Which stages a run executes: decided now, or by the worker after scanning the subject.
enum StagePlan {
    Fixed,
    PerSubject(Box<StageSettings>),
}

/// The batch subject's result from the events of its run.
fn subject_result(
    subject: &str,
    tally: &SubjectTally,
    policy: ReviewPolicy,
    cancelled: bool,
    elapsed: Duration,
    unexpected_end: bool,
) -> SubjectResult {
    let o = tally.outcome(subject, policy, cancelled, elapsed);
    let (phase, message) = if unexpected_end && o.phase != BatchPhase::Cancelled {
        (
            BatchPhase::Failed,
            Some("the pipeline stopped without reporting the end of the run".to_owned()),
        )
    } else {
        (o.phase, o.message)
    };
    SubjectResult {
        subject: subject.to_owned(),
        phase,
        ok: o.stages_ok,
        failed: o.stages_failed,
        skipped: o.stages_skipped,
        message,
        awaiting: o.awaiting_stage,
        seconds: elapsed.as_secs_f64(),
        rework: false,
    }
}

struct PendingOverwrite {
    req: RunRequest,
    stages: Vec<Stage>,
    bids_dir: String,
    subject: Option<String>,
    /// The subjects of a run of several subjects; `None` for the viewed subject alone.
    batch: Option<Vec<String>>,
}

impl PendingOverwrite {
    fn belongs_to(&self, bids_dir: &str, subject: Option<&str>) -> bool {
        self.bids_dir == bids_dir.trim()
            && (self.batch.is_some() || self.subject.as_deref() == subject)
    }
}

fn quit_needs_confirm(running: bool, refine_dirty: bool, importing: bool, approved: bool) -> bool {
    (running || refine_dirty || importing) && !approved
}

struct BusyGuard(Arc<AtomicUsize>);

impl BusyGuard {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(counter))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) const SUBJECT_LOCKED: &str =
    "A run is in progress. Wait until it has finished or stop it, then change the dataset or subject.";

pub(crate) const BATCH_ACTIVE: &str = "A run of several subjects is still going (or its \
     registration reviews are open). Wait for it, stop it, or press \"End the batch\" in the \
     Pipeline panel first.";

pub(crate) const IMPORT_LOCKED_RUN: &str =
    "A run is in progress. Wait until it has finished or stop it, then import the images.";

pub(crate) const IMPORT_LOCKED_BATCH: &str = "A run of several subjects is still going (or its \
     registration reviews are open). Wait for it, stop it, or press \"End the batch\" in the \
     Pipeline panel, then import the images.";

pub(crate) const IMPORT_LOCKED_REVIEW: &str = "The registration review is open. Approve, reject \
     or close it first, then import the images: a decision made after the import would apply to \
     the old images.";

pub(crate) const IMPORT_LOCKED_REVIEW_FINISHING: &str =
    "Wait until the clean-up after the registration review has finished, then import the images.";

pub(crate) const IMPORT_LOCKED_REFINE: &str = "Refine localization is open. Save or discard the \
     corrections and close it first, then import the images: a correction saved after the import \
     would apply to the old images.";

pub(crate) const IMPORT_LOCKED_REFINE_SAVING: &str =
    "Wait until the corrected lead has been saved, then import the images.";

pub(crate) const IMPORT_LOCKED_GROUP: &str = "The group analysis is computing volumes of tissue \
     activated. Wait until it has finished or cancel it, then import the images.";

pub(crate) const RUN_WAIT_IMPORT: &str =
    "Wait until the image import has finished writing into the dataset.";

pub(crate) const RUN_ALREADY_ACTIVE: &str =
    "A run is already in progress. Wait until it has finished or stop it (Ctrl+.) first.";

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ImportBlockState {
    pub running: bool,
    pub batch_active: bool,
    pub review_open: bool,
    pub review_finishing: bool,
    pub refine_open: bool,
    pub refine_saving: bool,
    pub group_calculating: bool,
}

pub(crate) fn import_block(s: ImportBlockState) -> Option<&'static str> {
    if s.batch_active {
        Some(IMPORT_LOCKED_BATCH)
    } else if s.running {
        Some(IMPORT_LOCKED_RUN)
    } else if s.review_finishing {
        Some(IMPORT_LOCKED_REVIEW_FINISHING)
    } else if s.review_open {
        Some(IMPORT_LOCKED_REVIEW)
    } else if s.refine_saving {
        Some(IMPORT_LOCKED_REFINE_SAVING)
    } else if s.refine_open {
        Some(IMPORT_LOCKED_REFINE)
    } else if s.group_calculating {
        Some(IMPORT_LOCKED_GROUP)
    } else {
        None
    }
}

pub(crate) const OUTPUT_LOCKED_RUN: &str =
    "A run is in progress. Wait until it has finished or stop it, then change the output directory.";
pub(crate) const OUTPUT_LOCKED_SESSION: &str = "Close Registration review and Refine localization \
     first: they show the results of the current output directory.";

const STIMULATION_SETTLE_TIMEOUT_FRAMES: u32 = 36_000;

pub(crate) const OUTPUT_LOCKED_STIMULATION: &str =
    "Close the stimulation editor first: it shows the settings of the current output directory.";

pub(crate) const RUN_WAIT_PREPARING: &str = "Wait until the registration review images are ready.";
pub(crate) const RUN_WAIT_FINISHING: &str = "Wait until the review clean-up has finished.";

type PreparedReview = (Stage, Result<dbsloc_pipeline::ReviewRequest, String>);

type PreparedRefine = Result<
    (
        dbsloc_viz::refine_app::RefinePanel,
        RefineOwner,
        dbsloc_pipeline::options::RunOptions,
    ),
    String,
>;

enum RefineSaveMsg {
    Log(String),
    Done(Result<dbsloc_pipeline::manual_correction::CorrectionReport, String>),
}

pub(crate) const RUN_WAIT_REFINE_PREPARING: &str =
    "Wait until Refine localization has opened the lead.";
pub(crate) const RUN_WAIT_REFINE_DIRTY: &str = "Save or discard the lead corrections first.";

fn refine_owner_matches(owner: &RefineOwner, bids_dir: &str, selected: Option<&str>) -> bool {
    let subject = selected.map(|s| s.strip_prefix("sub-").unwrap_or(s));
    owner.bids_dir == Path::new(bids_dir) && subject == Some(owner.subject.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedRefineAction {
    Open,
    OpenQuietly,
    Drop,
    DropAndRetry,
    RefuseRun,
    RetryAfterJob,
}

#[derive(Debug, Clone, Copy, Default)]
struct PreparedRefineState {
    follow: bool,
    current: bool,
    awaited: bool,
    running: bool,
    importing: bool,
    job_since_start: bool,
}

fn importing_edge(was: bool, now: bool, epoch: u64) -> (bool, u64) {
    (
        now,
        if now && !was {
            epoch.wrapping_add(1)
        } else {
            epoch
        },
    )
}

fn prepared_refine_action(s: PreparedRefineState) -> PreparedRefineAction {
    if !s.follow {
        return if !s.current {
            PreparedRefineAction::Drop
        } else if s.running {
            PreparedRefineAction::RefuseRun
        } else {
            PreparedRefineAction::Open
        };
    }
    if !s.current || !s.awaited {
        PreparedRefineAction::DropAndRetry
    } else if s.running || s.importing {
        PreparedRefineAction::RetryAfterJob
    } else if s.job_since_start {
        PreparedRefineAction::DropAndRetry
    } else {
        PreparedRefineAction::OpenQuietly
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct RefineFollowJobs {
    run_active: bool,
    preparing: bool,
    saving: bool,
    review_preparing: bool,
    review_finishing: bool,
    importing: bool,
}

fn refine_prepare_blocks(preparing: bool, follow: bool) -> bool {
    preparing && !follow
}

fn refine_follow_busy(j: RefineFollowJobs) -> bool {
    j.run_active
        || j.preparing
        || j.saving
        || j.review_preparing
        || j.review_finishing
        || j.importing
}

pub(crate) const RUN_WAIT_REFINE_SAVING: &str = "Wait until the corrected lead has been saved.";

#[derive(Debug, Clone, Copy, Default)]
struct RunWaitState {
    review_preparing: bool,
    review_finishing: bool,
    refine_saving: bool,
    refine_preparing: bool,
    refine_dirty: bool,
    importing: bool,
}

fn run_wait_for(s: RunWaitState) -> Option<&'static str> {
    if s.review_preparing {
        Some(RUN_WAIT_PREPARING)
    } else if s.review_finishing {
        Some(RUN_WAIT_FINISHING)
    } else if s.refine_saving {
        Some(RUN_WAIT_REFINE_SAVING)
    } else if s.refine_preparing {
        Some(RUN_WAIT_REFINE_PREPARING)
    } else if s.refine_dirty {
        Some(RUN_WAIT_REFINE_DIRTY)
    } else if s.importing {
        Some(RUN_WAIT_IMPORT)
    } else {
        None
    }
}

fn resolve_run_subject(
    opts: &dbsloc_pipeline::options::RunOptions,
) -> Result<dbsloc_pipeline::bids::Subject, dbsloc_pipeline::bids::BidsError> {
    dbsloc_pipeline::bids::resolve_subject_in(
        &opts.bids_dir,
        opts.output_dir.as_deref(),
        &opts.subject,
        true,
    )
}

fn prepare_refine_job(opts: dbsloc_pipeline::options::RunOptions) -> PreparedRefine {
    let sub = resolve_run_subject(&opts).map_err(|e| e.to_string())?;
    let context =
        dbsloc_pipeline::prepare_manual_correction(&sub, &opts).map_err(|e| e.to_string())?;
    let value = serde_json::to_value(&context).map_err(|e| e.to_string())?;
    let request: dbsloc_viz::refine_app::RefineRequest =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    let panel = dbsloc_viz::refine_app::RefinePanel::new(request).map_err(|e| e.to_string())?;
    let owner = RefineOwner {
        bids_dir: opts.bids_dir.clone(),
        subject: opts.subject.clone(),
    };
    Ok((panel, owner, opts))
}

fn pipeline_run_options(
    req: RunRequest,
    bids_dir: PathBuf,
    output_dir: Option<PathBuf>,
    subject: String,
    template: Option<PathBuf>,
    review_on: bool,
    both_postop: bool,
) -> dbsloc_pipeline::options::RunOptions {
    let postop_modality = if both_postop {
        crate::import_wizard::pipeline_postop_modality(req.postop_modality)
    } else {
        dbsloc_pipeline::bids::PostopModality::Auto
    };
    let template_t2 = req.normalization_template_t2.as_deref().map(PathBuf::from);
    dbsloc_pipeline::options::RunOptions {
        stages: req.stages,
        normalization_template: template,
        normalization_template_t2: template_t2,
        norm_multispectral: req.norm_multispectral,
        recon_method: req.recon_method.cli_value().to_owned(),
        recon_bright_metal: req.recon_bright_metal,
        el_model: req.el_model,
        ants_max_threads: req.ants_max_threads,
        norm_isotropic_mm: req.norm_grid,
        reg_backend_preprocess: req.reg_backend_preprocess,
        reg_backend_coregister: req.reg_backend_coregister,
        reg_backend_normalize: req.reg_backend_normalize,
        postop_modality,
        headless: true,
        checkreg: review_on,
        fingerprint_locks: true,
        no_scene3d: true,
        plan_only: false,
        stop_on_error: req.stop_on_error,
        overwrite_approved: req.overwrite_approved,
        output_dir,
        leaddbs_atlases_dir: req.leaddbs_atlases_dir,
        vat_labels: req.vat_labels,
        ..dbsloc_pipeline::options::RunOptions::new(bids_dir, subject)
    }
}

pub(crate) fn has_both_postop(r: &dbsloc_ui::PipelineReadiness) -> bool {
    r.has_raw_postop_ct && r.has_raw_postop_mr
}

fn stimulation_follow_options(
    stage_panel: &StagePanel,
    state: &DbslocAppState,
    readiness: &ReadinessCache,
    target: &crate::follow::Target,
) -> Result<dbsloc_pipeline::options::RunOptions, String> {
    let subject = target
        .subject
        .clone()
        .ok_or_else(|| "No subject is selected.".to_owned())?;
    let req = stage_panel.build_request(state, &[Stage::Vat]).ok_or_else(|| {
        "The settings of the volume of activated tissue step could not be read for this subject."
            .to_owned()
    })?;
    let template = state.normalization_template.as_deref().map(PathBuf::from);
    let both_postop = readiness.is_current_for(state) && has_both_postop(readiness.get());
    Ok(pipeline_run_options(
        req,
        target.bids_dir.clone(),
        state.output_dir(),
        subject,
        template,
        false,
        both_postop,
    ))
}

fn prepare_review_job(
    opts: &dbsloc_pipeline::options::RunOptions,
    stage: Stage,
) -> Result<dbsloc_pipeline::ReviewRequest, String> {
    let sub = resolve_run_subject(opts).map_err(|e| e.to_string())?;
    dbsloc_pipeline::prepare_review(&sub, opts, stage, &PngWriter::default())
}

fn finish_review_job(opts: &dbsloc_pipeline::options::RunOptions, stage: Stage) {
    if let Ok(sub) = resolve_run_subject(opts) {
        dbsloc_pipeline::finish_review(&sub, opts, stage);
    }
}

/// What `DBSLOC_APP_SCREENSHOT_ACTION` asked the window to show or do before the
/// capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenshotAction {
    Landing,
    Guided,
    Import,
    Connectomes,
    LeadDbsImport,
    ConnectomeCache,
    CacheConsent,
    Dock,
    Run,
    Focus,
    ViewerPopout,
    Viewer,
    Review,
    ReviewGuided,
    ReviewDuringRun,
    Refine,
    Stimulation,
    Group,
    Report,
    Help,
    ReviewPopout,
    RefinePopout,
    StimulationPopout,
    GroupPopout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewShot {
    Docked,
    Guided,
    Waiting,
}

impl ScreenshotAction {
    fn parse(value: &str) -> Self {
        match value.trim() {
            v if v == "guided" || v.starts_with("guided:") => Self::Guided,
            "dock" => Self::Dock,
            "import" => Self::Import,
            "connectomes" => Self::Connectomes,
            "leaddbs-import" => Self::LeadDbsImport,
            "connectome_cache" => Self::ConnectomeCache,
            "cache_consent" => Self::CacheConsent,
            "run" => Self::Run,
            "focus" => Self::Focus,
            "viewer-popout" => Self::ViewerPopout,
            "viewer" => Self::Viewer,
            "review" => Self::Review,
            "review-guided" => Self::ReviewGuided,
            "review-during-run" => Self::ReviewDuringRun,
            "refine" => Self::Refine,
            "stimulation" => Self::Stimulation,
            "group" => Self::Group,
            "review-popout" => Self::ReviewPopout,
            "refine-popout" => Self::RefinePopout,
            "stimulation-popout" => Self::StimulationPopout,
            "group-popout" => Self::GroupPopout,
            "help" => Self::Help,
            "report" => Self::Report,
            _ => Self::Landing,
        }
    }

    fn tab_popout(self) -> Option<TabKind> {
        match self {
            Self::ReviewPopout => Some(TabKind::Review),
            Self::RefinePopout => Some(TabKind::Refine),
            Self::StimulationPopout => Some(TabKind::Stimulation),
            Self::GroupPopout => Some(TabKind::Group),
            _ => None,
        }
    }

    fn review_shot(self) -> Option<ReviewShot> {
        match self {
            Self::Review | Self::ReviewPopout => Some(ReviewShot::Docked),
            Self::ReviewGuided => Some(ReviewShot::Guided),
            Self::ReviewDuringRun => Some(ReviewShot::Waiting),
            _ => None,
        }
    }
}

fn screenshot_mode_from(png: Option<&std::ffi::OsStr>, action: Option<&str>) -> bool {
    png.is_some_and(|p| !p.is_empty()) || action.is_some_and(|a| !a.trim().is_empty())
}

pub fn screenshot_mode() -> bool {
    screenshot_mode_from(
        std::env::var_os("DBSLOC_APP_SCREENSHOT_PNG").as_deref(),
        std::env::var("DBSLOC_APP_SCREENSHOT_ACTION")
            .ok()
            .as_deref(),
    )
}

pub fn screenshot_persistence_path() -> PathBuf {
    std::env::temp_dir()
        .join("dbsloc_app_screenshot")
        .join("app.ron")
}

const REVIEW_READY_TIMEOUT_FRAMES: u32 = 1200;
const REVIEW_READY_SETTLE_FRAMES: u32 = 15;

/// How long the dataset / subject / template inputs must hold still before a
/// filesystem scan is started for them.
const SCAN_DEBOUNCE: Duration = Duration::from_millis(400);

const QUIT_RUNNER_WAIT: Duration = Duration::from_secs(3);

const POPOUT_SCENE_TIMEOUT_FRAMES: u32 = 1800;
const POPOUT_HOLD: Duration = Duration::from_secs(12);

fn popout_screenshot_path(png: &Path) -> PathBuf {
    let stem = png
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    png.with_file_name(format!("{stem}-popout.png"))
}

pub(crate) fn config_base_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
    }
}

fn state_path_in(base: &Path) -> PathBuf {
    base.join("dbsloc").join("state.json")
}

fn app_state_path() -> Option<PathBuf> {
    config_base_dir().map(|base| state_path_in(&base))
}

fn load_app_state() -> DbslocAppState {
    let path = if screenshot_mode() {
        std::env::var_os("DBSLOC_APP_SCREENSHOT_STATE")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    } else {
        app_state_path()
    };
    let Some(path) = path else {
        return DbslocAppState::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return DbslocAppState::default();
    };
    let (mut state, complete) = parse_app_state(&text);
    if !complete && !screenshot_mode() {
        let backup = path.with_extension("json.bak");
        let kept = std::fs::write(&backup, &text).is_ok();
        crate::app_log::warn(&format!(
            "the saved settings in {} could not be read completely; the readable parts were kept{}",
            path.display(),
            if kept {
                format!(" and the file was copied to {}", backup.display())
            } else {
                String::new()
            }
        ));
    }
    state.stage.migrate_legacy_engine();
    state
}

fn zero_unlaid_floats(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => items.iter_mut().for_each(zero_unlaid_floats),
        serde_json::Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if inner.is_null() && (key == "x" || key == "y") {
                    *inner = serde_json::json!(0.0);
                } else {
                    zero_unlaid_floats(inner);
                }
            }
        }
        _ => {}
    }
}

fn state_loads(value: &serde_json::Value) -> bool {
    serde_json::from_value::<DbslocAppState>(value.clone()).is_ok()
}

fn object_at<'a>(
    root: &'a mut serde_json::Value,
    path: &[String],
) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
    let mut cur = root;
    for key in path {
        cur = cur.get_mut(key)?;
    }
    cur.as_object_mut()
}

fn merge_readable_fields(
    root: &mut serde_json::Value,
    path: &[String],
    loaded: serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in loaded {
        let mut trial = root.clone();
        if let Some(slot) = object_at(&mut trial, path) {
            slot.insert(key.clone(), value.clone());
        }
        if state_loads(&trial) {
            *root = trial;
            continue;
        }
        let serde_json::Value::Object(inner) = value else {
            continue;
        };
        let nested_object = object_at(root, path)
            .and_then(|m| m.get(&key))
            .is_some_and(serde_json::Value::is_object);
        if nested_object {
            let mut nested = path.to_vec();
            nested.push(key);
            merge_readable_fields(root, &nested, inner);
        }
    }
}

pub(crate) fn parse_app_state(text: &str) -> (DbslocAppState, bool) {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) else {
        return (DbslocAppState::default(), false);
    };
    if let Some(dock) = value.get_mut("dock") {
        zero_unlaid_floats(dock);
    }
    if let Ok(state) = serde_json::from_value::<DbslocAppState>(value.clone()) {
        return (state, true);
    }
    let serde_json::Value::Object(loaded) = value else {
        return (DbslocAppState::default(), false);
    };
    let Ok(mut merged) = serde_json::to_value(DbslocAppState::default()) else {
        return (DbslocAppState::default(), false);
    };
    merge_readable_fields(&mut merged, &[], loaded);
    (serde_json::from_value(merged).unwrap_or_default(), false)
}

pub(crate) fn state_to_json(state: &DbslocAppState) -> String {
    let Ok(mut value) = serde_json::to_value(state) else {
        return String::new();
    };
    if let Some(dock) = value.get_mut("dock") {
        zero_unlaid_floats(dock);
    }
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Text sizes and spacing, applied to **both** the light and the dark style so a
/// theme switch never resets them (and never needs re-applying per frame).
fn configure_styles(ctx: &egui::Context) {
    ctx.style_mut_of(egui::Theme::Dark, |style| tokens::apply(style, true));
    ctx.style_mut_of(egui::Theme::Light, |style| tokens::apply(style, false));
    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            dbsloc_viz::brand::heading_font_id(20.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::new(15.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Monospace,
            egui::FontId::new(14.0, egui::FontFamily::Monospace),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
        );
        style.spacing.interact_size = egui::vec2(26.0, 26.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(9.0, 4.0);
        style.spacing.menu_margin = egui::Margin::same(6);
        let body = style
            .text_styles
            .get(&egui::TextStyle::Body)
            .map(|f| f.size)
            .unwrap_or(15.0);
        style.spacing.slider_width = body * 11.0;
        style.interaction.tooltip_delay = 0.15;
        style.visuals.window_corner_radius = egui::CornerRadius::same(6);
        style.visuals.menu_corner_radius = egui::CornerRadius::same(6);
        style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(4);
        style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(4);
        style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(4);
        style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(4);
    });
}

fn show_logo(ui: &mut egui::Ui, logo: &egui::TextureHandle) {
    show_logo_sized(ui, logo, em(ui) * 1.6);
}

pub(crate) fn show_logo_sized(ui: &mut egui::Ui, logo: &egui::TextureHandle, side: f32) {
    let tint = palette(ui).text;
    ui.add(
        egui::Image::from_texture(logo)
            .fit_to_exact_size(egui::vec2(side, side))
            .tint(tint)
            .sense(egui::Sense::hover()),
    )
    .on_hover_text("Wyss Center for Bio and Neuroengineering");
}

struct Shortcut {
    keys: &'static str,
    action: &'static str,
}

const SHORTCUTS: &[(&str, &[Shortcut])] = &[
    (
        "Pipeline",
        &[
            Shortcut {
                keys: "Ctrl+R",
                action: "Run the selected steps",
            },
            Shortcut {
                keys: "Ctrl+.",
                action: "Stop the run, or stop saving a lead correction",
            },
            Shortcut {
                keys: "Ctrl+O",
                action: "Choose the input directory",
            },
            Shortcut {
                keys: "F5",
                action: "Scan the input directory again",
            },
            Shortcut {
                keys: "Alt+Up / Alt+Down",
                action: "Previous / next subject",
            },
        ],
    ),
    (
        "Workspace",
        &[
            Shortcut {
                keys: "Ctrl+1 to Ctrl+6",
                action: "Focus Subjects / Pipeline / Output / 3D Viewer / Registration review / Refine localization",
            },
            Shortcut {
                keys: "Ctrl+7",
                action: "Open Group analysis",
            },
            Shortcut {
                keys: "Ctrl+H",
                action: "Go to the start screen",
            },
            Shortcut {
                keys: "Ctrl+I",
                action: "Import images into a subject",
            },
            Shortcut {
                keys: "Ctrl+L",
                action: "Clear the output log",
            },
            Shortcut {
                keys: "Ctrl+Shift+C",
                action: "Copy the visible output log",
            },
            Shortcut {
                keys: "F1",
                action: "Show this help",
            },
            Shortcut {
                keys: "Esc",
                action: "Close the front dialog",
            },
        ],
    ),
    (
        "3D Viewer",
        &[
            Shortcut {
                keys: "F11",
                action: "Open the 3D Viewer in a separate window; there, F11 toggles full screen and Esc leaves it",
            },
            Shortcut {
                keys: "R",
                action: "Reset the view",
            },
            Shortcut {
                keys: "1 / 2 / 3",
                action: "Toggle the axial / coronal / sagittal slice",
            },
            Shortcut {
                keys: "Up / Down",
                action: "Move to the previous / next atlas structure",
            },
            Shortcut {
                keys: "Space",
                action: "Show or hide the focused atlas structure",
            },
            Shortcut {
                keys: "Ctrl+S",
                action: "Save the rendered scene to an image",
            },
            Shortcut {
                keys: "Shift+Arrows",
                action: "Pan with the keyboard",
            },
        ],
    ),
];

/// Product name, version and git hash of this build.
pub(crate) fn version_line() -> String {
    format!(
        "{} {}",
        dbsloc_pipeline::version::PRODUCT,
        dbsloc_pipeline::version::version_string()
    )
}

/// The graphics adapter and registration engine of this session, at the end of the help window
/// so they can always be looked up.
fn system_group(ui: &mut egui::Ui, rows: &[(String, String)]) {
    ui.label(egui::RichText::new("System").strong());
    egui::Grid::new("system_rows")
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for (what, value) in rows {
                ui.label(what);
                ui.add(egui::Label::new(value).wrap());
                ui.end_row();
            }
        });
    ui.add_space(6.0);
}

fn shortcut_group(ui: &mut egui::Ui, group: &str, rows: &[(&str, &str)]) {
    let unit = em(ui);
    ui.label(egui::RichText::new(group).strong());
    egui::Grid::new(format!("shortcuts_{group}"))
        .num_columns(2)
        .spacing([18.0, 4.0])
        .min_col_width(unit * 7.0)
        .show(ui, |ui| {
            for (keys, action) in rows {
                ui.add(egui::Label::new(egui::RichText::new(*keys).monospace()).wrap());
                ui.add(egui::Label::new(*action).wrap());
                ui.end_row();
            }
        });
    ui.add_space(6.0);
}

fn sc(modifiers: egui::Modifiers, key: egui::Key) -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(modifiers, key)
}

impl DbslocApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        dbsloc_viz::brand::install_fonts(&cc.egui_ctx);
        configure_styles(&cc.egui_ctx);
        let logo = cc.egui_ctx.load_texture(
            "wyss-center-avatar",
            dbsloc_viz::brand::brand_logo_image(),
            dbsloc_viz::brand::logo_texture_options(),
        );
        let mut state = load_app_state();
        cc.egui_ctx.set_theme(state.theme.preference());
        if state.normalization_template.is_none() {
            state.normalization_template = dbsloc_pipeline::templates::select_template(None)
                .map(|r| r.path.to_string_lossy().into_owned());
        }
        if let Some(model) = state.stage.el_model.clone() {
            if !dbsloc_pipeline::electrode_model_names()
                .iter()
                .any(|n| *n == model)
            {
                state.stage.el_model = None;
            }
        }
        let screenshot_value = std::env::var("DBSLOC_APP_SCREENSHOT_ACTION").unwrap_or_default();
        let screenshot_action = ScreenshotAction::parse(&screenshot_value);
        let import = (screenshot_action == ScreenshotAction::Import).then(|| {
            let mut wizard = ImportWizard::new(&state.bids_dir);
            wizard.output_dirs = state.output_dirs.clone();
            wizard
        });
        let screenshot_step = screenshot_value
            .trim()
            .strip_prefix("guided:")
            .and_then(GuidedStep::from_name);
        let bids_dir = state.bids_dir.clone();
        Self::remember_bids_dir_in_state(&mut state, &bids_dir);
        let mut dock = state.dock.take().unwrap_or_else(default_dock_state);
        remove_transient_tabs(&mut dock);
        let theme = state.theme;
        let last_saved_state = state_to_json(&state);
        let probe_ctx = cc.egui_ctx.clone();
        thread::spawn(move || {
            dbsloc_pipeline::reg_backend::cached_probe();
            probe_ctx.request_repaint();
        });
        let stage_panel = StagePanel::default();
        let render_adapter = cc
            .wgpu_render_state
            .as_ref()
            .map(|rs| dbsloc_viz::gpu_setup::RenderAdapter::from_info(&rs.adapter.get_info()));
        if let Some(r) = &render_adapter {
            eprintln!("dbs-localizer: drawing on {}", r.label());
            crate::app_log::info(&format!("drawing on {}", r.label()));
        }
        stage_panel.gpu_help.set_render(render_adapter);
        let first_start_import = crate::leaddbs_import::begin_first_start(
            &mut state,
            screenshot_action == ScreenshotAction::Landing,
            screenshot_mode(),
        );
        Self {
            state,
            dock,
            logo,
            subject_panel: SubjectPanel::default(),
            stage_panel,
            log_panel: LogPanel::default(),
            scene_panel: ScenePanel::default(),
            review_panel: ReviewPanel::default(),
            review_rx: None,
            review_prepare: None,
            run_owner: None,
            prepare_owner: None,
            review_finishing: Arc::new(AtomicUsize::new(0)),
            refine_tab: RefineTab::default(),
            stimulation_tab: crate::stimulation_panel::StimulationTab::default(),
            fibre_files: crate::connectivity::FibreFiles::default(),
            group_tab: crate::group_panel::GroupTab::default(),
            group_look_pending: None,
            group_layers_pending: Vec::new(),
            group_layers_view_started: false,
            screenshot_group: matches!(
                screenshot_action,
                ScreenshotAction::Group | ScreenshotAction::GroupPopout
            ),
            group_scene_since: None,
            stimulation_scene: None,
            screenshot_stimulation: matches!(
                screenshot_action,
                ScreenshotAction::Stimulation | ScreenshotAction::StimulationPopout
            ),
            stimulation_scene_since: None,
            refine_prepare: None,
            refine_save: None,
            refine_opts: None,
            refine_prepare_owner: None,
            refine_prepare_follow: false,
            refine_prepare_epoch: 0,
            job_epoch: 0,
            job_epoch_importing: false,
            follow_readiness_seen: 0,
            refine_cancel: Arc::new(AtomicBool::new(false)),
            refine_busy: Arc::new(AtomicUsize::new(0)),
            screenshot_refine: matches!(
                screenshot_action,
                ScreenshotAction::Refine | ScreenshotAction::RefinePopout
            ),
            readiness: ReadinessCache::default(),
            gpu_probe_seen: false,
            progress: RunProgress::default(),
            _runner_thread: None,
            _relay_thread: None,
            pipeline_cancel: Arc::new(AtomicBool::new(false)),
            pending_bids_pick: None,
            pending_output_pick: None,
            pending_template_pick: None,
            pending_template_t2_pick: None,
            pending_manifest_pick: None,
            pending_backdrop_pick: None,
            last_saved_state,
            last_persist_check: Instant::now(),
            applied_theme: Some(theme),
            open_viewer_after_reconstruct: false,
            errors: Vec::new(),
            show_shortcuts: screenshot_action == ScreenshotAction::Help,
            window_was_focused: true,
            gpu_status_logged: false,
            confirm_overwrite: None,
            quit_after_run: false,
            frames: 0,
            screenshot_requested: false,
            screenshot_png: std::env::var_os("DBSLOC_APP_SCREENSHOT_PNG")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty()),
            screenshot_action_run: screenshot_action == ScreenshotAction::Run,
            screenshot_action_focus: screenshot_action == ScreenshotAction::Focus,
            screenshot_action_popout: screenshot_action == ScreenshotAction::ViewerPopout,
            screenshot_viewer_docked: screenshot_action == ScreenshotAction::Viewer,
            popout_since: None,
            screenshot_tab_popout: screenshot_action.tab_popout(),
            screenshot_popout_pending: false,
            screenshot_mode: screenshot_mode(),
            screenshot_review: screenshot_action.review_shot(),
            review_ready_since: None,
            screenshot_close_at: None,
            scan_key: None,
            scan_settled_at: Instant::now() - SCAN_DEBOUNCE,
            confirm_quit: false,
            quit_waits_for_import: false,
            quit_cancels_import: false,
            quit_approved: false,
            show_landing: screenshot_action == ScreenshotAction::Landing,
            guided: screenshot_action == ScreenshotAction::Guided,
            guided_step: screenshot_step.unwrap_or(GuidedStep::FIRST_WORK),
            guided_vta_want: None,
            guided_vta_applied: None,
            guided_vta_load: None,
            landing: LandingView {
                subjects_open: screenshot_value.trim() == "landing:subjects",
                import_block: None,
            },
            open_viewer_after_visualize: false,
            import,
            report_import: crate::report_import::ReportImportHost::default(),
            pending_report_pick: None,
            screenshot_report: screenshot_action == ScreenshotAction::Report,
            image_preview: crate::panels::ImagePreviewHost::default(),
            leaddbs_import: {
                let mut window = crate::leaddbs_import::LeadDbsImport::default();
                if screenshot_action == ScreenshotAction::LeadDbsImport {
                    match std::env::var_os("DBSLOC_APP_LEADDBS_FOLDER").filter(|p| !p.is_empty()) {
                        Some(dir) => window.open_with_folder(
                            PathBuf::from(dir),
                            std::env::var("DBSLOC_APP_LEADDBS_MODE").is_ok_and(|m| m == "copy"),
                        ),
                        None => window.open(false),
                    }
                } else if first_start_import {
                    window.open(true);
                }
                window
            },
            connectomes_setting_push: crate::connectome_roots::SettingPush::default(),
            screenshot_leaddbs: screenshot_action == ScreenshotAction::LeadDbsImport,
            connectome_panel: {
                let mut panel = crate::connectome_panel::ConnectomePanel::default();
                if screenshot_action == ScreenshotAction::Connectomes {
                    panel.open();
                }
                if screenshot_action == ScreenshotAction::ConnectomeCache {
                    panel.open_cache_manager();
                }
                panel
            },
            screenshot_consent: (screenshot_action == ScreenshotAction::CacheConsent).then(|| {
                crate::connectome_cache_ui::ConsentDialog::new(
                    crate::connectome_cache_ui::fake_request(),
                )
            }),
            batch: None,
            batch_plan_rx: None,
            last_batch: None,
            confirm_batch_stop: false,
        }
    }

    fn remember_bids_dir_in_state(state: &mut DbslocAppState, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        state.bids_dir = path.to_owned();
        state.recent_bids_dirs.retain(|p| p != path);
        state.recent_bids_dirs.insert(0, path.to_owned());
        state.recent_bids_dirs.truncate(RECENT_LIMIT);
    }

    fn remember_bids_dir(&mut self, path: &str) {
        Self::remember_bids_dir_in_state(&mut self.state, path);
    }

    fn log_gpu_status_once(&mut self) {
        if self.gpu_status_logged || !self.stage_panel.gpu_help.probe_known() {
            return;
        }
        self.gpu_status_logged = true;
        let rows = self.stage_panel.gpu_help.system_rows();
        let text = rows
            .iter()
            .map(|(what, value)| format!("{what}: {value}"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        crate::app_log::info(&text);
    }

    fn push_error(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let (title, body) = (title.into(), crate::panels::humanize_message(&body.into()));
        let line = format!("{title}: {body}");
        if add_error(&mut self.errors, title, body) {
            self.log_panel.push_error(line);
        }
    }

    fn subject_derivatives_dir(&self) -> Option<PathBuf> {
        let subject = self.state.selected_subject.as_ref()?;
        self.state.subject_derivatives_dir(subject)
    }

    /// Persist at most once a second, and only when something actually changed.
    fn persist_state_if_changed(&mut self, force: bool) {
        if self.screenshot_mode {
            return;
        }
        if !force && self.last_persist_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_persist_check = Instant::now();
        if let Some(view) = self.scene_panel.capture_view_state() {
            self.state.scene.view = Some(view);
        }
        self.state.dock = Some(self.dock.clone());
        let json = state_to_json(&self.state);
        self.state.dock = None;
        if json == self.last_saved_state {
            return;
        }
        if let Some(path) = app_state_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if write_atomically(&path, &json).is_ok() {
                self.last_saved_state = json;
            }
        }
    }

    /// Spawn the pipeline on a worker thread. A relay thread forwards each event
    /// to the UI **and** requests a repaint, so the log updates without polling.
    fn ask_overwrite(&mut self, req: RunRequest, stages: Vec<Stage>) {
        self.confirm_overwrite = Some(PendingOverwrite {
            req,
            stages,
            bids_dir: self.state.bids_dir.trim().to_owned(),
            subject: self.state.selected_subject.clone(),
            batch: None,
        });
    }

    fn run_refusal(&self) -> Option<&'static str> {
        if self.stage_panel.running {
            Some(RUN_ALREADY_ACTIVE)
        } else if self.batch.is_some() {
            Some(BATCH_ACTIVE)
        } else if let Some(wait) = self.run_wait() {
            Some(wait)
        } else if self.state.selected_subject.is_none() {
            Some("Select a subject first.")
        } else {
            None
        }
    }

    fn spawn_run(&mut self, req: RunRequest, ctx: &egui::Context) -> bool {
        match self.try_spawn_run(req, ctx) {
            Ok(()) => true,
            Err(reason) => {
                self.push_error("Cannot start the run", reason);
                false
            }
        }
    }

    fn try_spawn_run(
        &mut self,
        mut req: RunRequest,
        ctx: &egui::Context,
    ) -> Result<(), &'static str> {
        if let Some(reason) = self.run_refusal() {
            return Err(reason);
        }
        let Some(subj) = self.state.selected_subject.clone() else {
            return Err("Select a subject first.");
        };
        req.stages = stages_with_precoreg(&req.stages, self.readiness.get());
        self.close_refine_for(&req.stages);
        self.log_panel.prefix = None;
        self.log_panel.defer_review = false;
        let kind = if req.review_registrations {
            ReviewKind::Hook
        } else {
            ReviewKind::Off
        };
        self.launch_run(req, subj, StagePlan::Fixed, kind, ctx);
        Ok(())
    }

    fn close_refine_for(&mut self, stages: &[Stage]) {
        if stages
            .iter()
            .any(|s| matches!(s, Stage::Reconstruct | Stage::Orientation))
            && self.refine_tab.has_session()
        {
            self.refine_tab.close_with_notice(
                "Refine localization was closed because the run recomputes the lead. Open it \
                 again when the run has finished.",
            );
            self.refine_opts = None;
            self.log_panel
                .push("Refine localization closed: the run recomputes the lead");
        }
    }

    /// Start the pipeline for `subj` (a `sub-<id>` folder name) on a worker thread.
    fn launch_run(
        &mut self,
        req: RunRequest,
        subj: String,
        plan: StagePlan,
        kind: ReviewKind,
        ctx: &egui::Context,
    ) {
        let bids = self.state.bids_dir.clone();
        self.remember_bids_dir(&bids);
        let subject = subj.strip_prefix("sub-").unwrap_or(&subj).to_owned();
        let bids_dir = PathBuf::from(&self.state.bids_dir);
        let output_dir = self.state.output_dir();
        self.run_owner = Some(ReviewOwner {
            bids_dir: bids_dir.clone(),
            subject: subject.clone(),
        });
        let template = self
            .state
            .normalization_template
            .as_deref()
            .map(PathBuf::from);

        self.pipeline_cancel.store(false, Ordering::SeqCst);
        self.review_panel.end_run();
        let review = match kind {
            ReviewKind::Hook => {
                let (to_ui, from_pipeline) = mpsc::channel::<BridgeMsg>();
                self.review_rx = Some(from_pipeline);
                Some(ReviewSetup {
                    hook: crate::review::make_hook(to_ui, ctx.clone()),
                    png: PngWriter::default(),
                })
            }
            ReviewKind::Defer => {
                self.review_rx = None;
                Some(crate::review::defer_setup())
            }
            ReviewKind::Off => {
                self.review_rx = None;
                None
            }
        };
        let review_on = review.is_some();
        let defer = kind == ReviewKind::Defer;
        let plan_tx = match &plan {
            StagePlan::Fixed => {
                self.batch_plan_rx = None;
                self.progress.start_with(&req.stages, defer);
                None
            }
            StagePlan::PerSubject(_) => {
                let (tx, rx) = mpsc::channel::<Vec<Stage>>();
                self.batch_plan_rx = Some(rx);
                self.progress.start_with(&[], defer);
                Some(tx)
            }
        };
        crate::app_log::info(&format!(
            "run started: subject {subject}; steps {}; engine preprocess {:?}, coregister {:?}, normalize {:?}; review {}",
            req.stages
                .iter()
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join(", "),
            req.reg_backend_preprocess,
            req.reg_backend_coregister,
            req.reg_backend_normalize,
            req.review_registrations
        ));
        self.open_viewer_after_reconstruct =
            req.open_viewer_after_reconstruct && req.stages.contains(&Stage::Reconstruct);
        self.stage_panel.running = true;
        self.stage_panel.stopping = false;
        self.job_epoch = self.job_epoch.wrapping_add(1);

        let (tx_raw, rx_raw) = mpsc::channel::<RunEvent>();
        let (tx_ui, rx_ui) = mpsc::channel::<RunEvent>();
        self.log_panel.attach(rx_ui);

        let ctx_relay = ctx.clone();
        self._relay_thread = Some(thread::spawn(move || {
            while let Ok(ev) = rx_raw.recv() {
                if tx_ui.send(ev).is_err() {
                    break;
                }
                ctx_relay.request_repaint();
            }
            ctx_relay.request_repaint();
        }));

        let cancel = Arc::clone(&self.pipeline_cancel);
        let subject_folder = subj;
        self._runner_thread = Some(thread::spawn(move || {
            let mut req = req;
            let template_text = template
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned());
            let mut scanned = None;
            if let (StagePlan::PerSubject(settings), Some(plan_tx)) = (plan, plan_tx) {
                let readiness = crate::panels::scan_readiness(
                    &bids_dir.to_string_lossy(),
                    output_dir.as_deref(),
                    &subject_folder,
                    template_text.as_deref(),
                );
                req.stages = stages_with_precoreg(
                    &settings.selected_runnable_stages(&readiness),
                    &readiness,
                );
                let _ = plan_tx.send(req.stages.clone());
                scanned = Some(readiness.clone());
                if req.stages.is_empty() {
                    let _ = tx_raw.send(RunEvent::LogLine {
                        line: "Nothing to run: every selected step already has output for this \
                               subject. Tick \"Rerun all selected steps\" to run them again."
                            .to_owned(),
                    });
                    let _ = tx_raw.send(RunEvent::PipelineCompleted {
                        stages_ok: 0,
                        stages_failed: 0,
                        stages_skipped: 0,
                    });
                    return;
                }
            }
            let both_postop = match scanned {
                Some(r) => has_both_postop(&r),
                None if req.postop_modality != dbsloc_ui::PostopModality::Auto => {
                    has_both_postop(&crate::panels::scan_readiness(
                        &bids_dir.to_string_lossy(),
                        output_dir.as_deref(),
                        &subject_folder,
                        template_text.as_deref(),
                    ))
                }
                None => false,
            };
            let opts = pipeline_run_options(
                req,
                bids_dir,
                output_dir,
                subject,
                template,
                review_on,
                both_postop,
            );
            let _ = match review {
                Some(review) => {
                    run_pipeline_channel_with_review(&opts, &tx_raw, Some(cancel.as_ref()), review)
                }
                None => run_pipeline_channel(&opts, &tx_raw, Some(cancel.as_ref())),
            };
        }));
    }

    fn batch_subjects(&self) -> Vec<String> {
        self.state
            .checked_subjects
            .ordered(&self.state.bids_dir, self.subject_panel.subjects())
    }

    /// Run `req` for every ticked subject, after the checks that do not depend on one subject.
    fn request_batch(&mut self, req: RunRequest, ctx: &egui::Context) {
        if self.stage_panel.running {
            self.push_error("Cannot start the run", RUN_ALREADY_ACTIVE);
            return;
        }
        if self.batch.is_some() {
            self.push_error("Cannot start this run yet", BATCH_ACTIVE);
            return;
        }
        let blockers = self
            .state
            .stage
            .batch_blockers(self.readiness.get().has_bids_dir);
        if let Some(first) = blockers.first() {
            self.push_error("Cannot start this run yet", first.clone());
            self.focus_tab(TabKind::Pipeline);
            return;
        }
        let subjects = self.batch_subjects();
        if subjects.len() < 2 {
            self.spawn_run(req, ctx);
            return;
        }
        if self.state.stage.overwrite_results {
            self.confirm_overwrite = Some(PendingOverwrite {
                stages: req.stages.clone(),
                req,
                bids_dir: self.state.bids_dir.trim().to_owned(),
                subject: self.state.selected_subject.clone(),
                batch: Some(subjects),
            });
            return;
        }
        self.start_batch(req, subjects, ctx);
    }

    fn start_batch(&mut self, req: RunRequest, subjects: Vec<String>, ctx: &egui::Context) {
        if let Some(wait) = self.run_wait() {
            self.push_error("Cannot start this run yet", wait);
            return;
        }
        self.close_refine_for(&req.stages);
        let bids = self.state.bids_dir.clone();
        self.remember_bids_dir(&bids);
        self.log_panel.push(format!(
            "Run of {} subjects started ({}): {}",
            subjects.len(),
            req.review_mode.label().to_lowercase(),
            subjects.join(", ")
        ));
        crate::app_log::info(&format!(
            "batch started: {} subjects; review {:?}",
            subjects.len(),
            req.review_mode
        ));
        self.batch = Some(BatchRun {
            queue: BatchQueue::new(&subjects, req.review_mode),
            req,
            settings: self.state.stage.clone(),
            bids_dir: bids.trim().to_owned(),
            tally: SubjectTally::default(),
            started: Instant::now(),
        });
        self.last_batch = None;
        self.focus_tab(TabKind::Output);
        self.tick_batch(ctx);
    }

    /// Advance the batch whenever nothing runs: the next subject, then the next review, then
    /// the end.
    fn tick_batch(&mut self, ctx: &egui::Context) {
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        if self.stage_panel.running || b.queue.current().is_some() || self.review_prepare.is_some()
        {
            return;
        }
        if self.state.bids_dir.trim() != b.bids_dir {
            b.queue.abandon();
            self.log_panel
                .push("The dataset changed, so the run of several subjects was ended.");
        }
        if b.queue.reviewing().is_some() {
            if self.review_panel.session_stage().is_some() {
                return;
            }
            b.queue.skip_review(None);
        }
        if self.run_wait().is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
            return;
        }
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        if let Some(next) = b.queue.start_next() {
            self.spawn_batch_subject(next, ctx);
            return;
        }
        if let Some(item) = b.queue.next_review() {
            let more = b.queue.reviews_waiting();
            self.state.selected_subject = Some(item.subject.clone());
            self.log_panel.push(format!(
                "◔ {}: opening the {} review{}",
                item.subject,
                item.stage.label(),
                if more > 0 {
                    format!(" ({more} more after this one)")
                } else {
                    String::new()
                }
            ));
            self.start_review_prepare(item.stage, ctx);
            if self.review_prepare.is_none() {
                if let Some(b) = self.batch.as_mut() {
                    b.queue
                        .skip_review(Some("the registration review could not be opened"));
                }
                ctx.request_repaint();
            }
            return;
        }
        if b.queue.is_finished() {
            self.finish_batch();
        }
    }

    fn spawn_batch_subject(&mut self, next: QueuedRun, ctx: &egui::Context) {
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        b.tally = SubjectTally::default();
        b.started = Instant::now();
        let mut req = b.req.clone();
        let kind = match b.queue.mode() {
            ReviewMode::DuringRun => ReviewKind::Hook,
            ReviewMode::AfterAll => ReviewKind::Defer,
            ReviewMode::Off => ReviewKind::Off,
        };
        let plan = match next.stages {
            Some(stages) => {
                req.stages = stages;
                b.queue.set_planned(&req.stages);
                StagePlan::Fixed
            }
            None => StagePlan::PerSubject(Box::new(b.settings.clone())),
        };
        let label = b
            .queue
            .progress_label()
            .unwrap_or_else(|| next.subject.clone());
        self.state.selected_subject = Some(next.subject.clone());
        self.log_panel.push(format!("── {label} ──"));
        self.log_panel.prefix = Some(next.subject.clone());
        self.log_panel.defer_review = kind == ReviewKind::Defer;
        self.launch_run(req, next.subject, plan, kind, ctx);
    }

    /// Record the result of the batch subject whose run just ended.
    fn finish_batch_subject(&mut self, unexpected_end: bool) {
        let cancelled = self.pipeline_cancel.load(Ordering::SeqCst);
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        let Some(run) = b.queue.current().cloned() else {
            return;
        };
        let result = subject_result(
            &run.subject,
            &b.tally,
            b.policy(),
            cancelled,
            b.started.elapsed(),
            unexpected_end,
        );
        let pending = b.queue.pending_len();
        let line = format!(
            "{} {}: {} ({}){}",
            match result.phase {
                BatchPhase::Done => "✔",
                BatchPhase::AwaitingQc => "◔",
                BatchPhase::Cancelled => "⏹",
                BatchPhase::Failed | BatchPhase::PlanError => "✖",
            },
            run.subject,
            result.status_text(),
            result.counts_text(),
            result
                .message
                .as_deref()
                .map(|m| format!(" — {}", crate::panels::humanize_message(m)))
                .unwrap_or_default()
        );
        let failed = result.phase.is_failure();
        b.queue.finish_current(result);
        if cancelled {
            b.queue.stop_after_current();
        }
        let stop_requested = b.queue.stop_requested();
        self.batch_plan_rx = None;
        if failed {
            self.log_panel.push_error(line);
            if pending > 0 && !stop_requested {
                self.log_panel.push(format!(
                    "{} failed. Run this subject again once the cause is fixed. Moving on to the next subject.",
                    run.subject
                ));
            }
        } else {
            self.log_panel.push(line);
        }
    }

    fn finish_batch(&mut self) {
        let Some(b) = self.batch.take() else {
            return;
        };
        self.log_panel.prefix = None;
        self.log_panel.defer_review = false;
        let summary = b.queue.summary_line();
        self.log_panel.push(format!(
            "Run of several subjects finished in {}: {summary}",
            format_seconds(b.queue.results().iter().map(|r| r.seconds).sum())
        ));
        for r in b.queue.results() {
            self.log_panel.push(format!(
                "  {} · {} · {}{}",
                r.subject,
                r.status_text(),
                r.counts_text(),
                r.message
                    .as_deref()
                    .map(|m| format!(" · {}", crate::panels::humanize_message(m)))
                    .unwrap_or_default()
            ));
        }
        crate::app_log::info(&format!("batch finished: {summary}"));
        self.readiness.invalidate();
        self.last_batch = Some((b.queue.results().to_vec(), summary));
    }

    /// Stop button / Ctrl+. : a batch with subjects still queued asks how to stop.
    fn request_stop(&mut self) {
        let queued = self
            .batch
            .as_ref()
            .is_some_and(|b| b.queue.pending_len() > 0 && !b.queue.stop_requested());
        if queued {
            self.confirm_batch_stop = true;
            return;
        }
        self.pipeline_cancel.store(true, Ordering::SeqCst);
        self.stage_panel.stopping = true;
        self.log_panel
            .push("Stop requested — ending the current step and skipping the rest.");
    }

    fn batch_info(&self) -> BatchInfo {
        let subjects = if self.batch.is_some() || self.stage_panel.running {
            Vec::new()
        } else {
            self.batch_subjects()
        };
        match &self.batch {
            Some(b) => BatchInfo {
                subjects,
                progress: b.queue.progress_label(),
                review: b
                    .queue
                    .reviewing()
                    .map(|r| (r.subject.clone(), r.stage, b.queue.reviews_waiting())),
                results: b.queue.results().to_vec(),
                summary: Some(b.queue.summary_line()),
                active: true,
            },
            None => BatchInfo {
                subjects,
                progress: None,
                review: None,
                results: self
                    .last_batch
                    .as_ref()
                    .map(|l| l.0.clone())
                    .unwrap_or_default(),
                summary: self.last_batch.as_ref().map(|l| l.1.clone()),
                active: false,
            },
        }
    }

    fn open_bids_picker(&mut self, ctx: &egui::Context) {
        if self.pending_bids_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_bids_pick = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select the input directory")
                .pick_folder()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn open_output_picker(&mut self, ctx: &egui::Context) {
        if self.pending_output_pick.is_some() {
            return;
        }
        if let Some(reason) = self.output_folder_lock() {
            self.push_error("Cannot change the output directory now", reason);
            return;
        }
        let start = self
            .state
            .derivatives_root()
            .into_iter()
            .flat_map(|r| r.ancestors().map(Path::to_path_buf).collect::<Vec<_>>())
            .find(|p| p.is_dir());
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_output_pick = Some(rx);
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Select the output directory");
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let result = dialog
                .pick_folder()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Why the output directory cannot change right now, if it cannot.
    fn output_folder_lock(&self) -> Option<&'static str> {
        if self.stage_panel.running {
            return Some(OUTPUT_LOCKED_RUN);
        }
        if let Some(wait) = self.run_wait() {
            return Some(wait);
        }
        if self.review_panel.session_stage().is_some() || self.refine_tab.has_session() {
            return Some(OUTPUT_LOCKED_SESSION);
        }
        if self.stimulation_tab.is_open() || self.stimulation_tab.is_preparing() {
            return Some(OUTPUT_LOCKED_STIMULATION);
        }
        None
    }

    /// Choose (`Some`) or reset (`None`) the output directory of the current dataset.
    fn change_output_dir(&mut self, dir: Option<&str>) {
        if let Some(reason) = self.output_folder_lock() {
            self.push_error("Cannot change the output directory now", reason);
            return;
        }
        let before = self.state.output_dir();
        self.state.set_output_dir(dir);
        if let Some(chosen) = self.state.output_dir() {
            remember_output_dir_in_state(&mut self.state, &chosen.to_string_lossy());
        }
        if self.state.output_dir() == before {
            return;
        }
        if let Some(root) = self.state.derivatives_root() {
            self.log_panel
                .push(format!("Output directory: {}", root.display()));
        }
        self.readiness.invalidate();
    }

    fn open_template_picker(&mut self, ctx: &egui::Context) {
        if self.pending_template_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_template_pick = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select the MNI template NIfTI file")
                .add_filter("NIfTI", &["nii", "nii.gz", "gz"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn open_template_t2_picker(&mut self, ctx: &egui::Context) {
        if self.pending_template_t2_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_template_t2_pick = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select the T2 template NIfTI file")
                .add_filter("NIfTI", &["nii", "nii.gz", "gz"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn open_manifest_picker(&mut self, ctx: &egui::Context) {
        if self.pending_manifest_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_manifest_pick = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select a scene manifest JSON file")
                .add_filter("JSON", &["json"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn open_backdrop_picker(&mut self, ctx: &egui::Context) {
        if self.pending_backdrop_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_backdrop_pick = Some(rx);
        thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select an image for the slices (it must be in the template space)")
                .add_filter("NIfTI image", &["nii", "gz"])
                .pick_file();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn poll_file_picks(&mut self) {
        if let Some(ref rx) = self.pending_backdrop_pick {
            if let Ok(pick) = rx.try_recv() {
                self.pending_backdrop_pick = None;
                if let Some(path) = pick {
                    if self.scene_panel.accepts_backdrop_pick() {
                        if is_nifti_file(&path) {
                            self.scene_panel.add_custom_backdrop(&path);
                        } else {
                            self.push_error("3D viewer", "Choose a NIfTI image (.nii or .nii.gz).");
                        }
                    }
                }
            }
        }
        if let Some(ref rx) = self.pending_bids_pick {
            if let Ok(pick) = rx.try_recv() {
                if let Some(path) = pick {
                    self.remember_bids_dir(&path);
                    self.landing.subjects_open = true;
                }
                self.pending_bids_pick = None;
            }
        }
        if let Some(ref rx) = self.pending_output_pick {
            if let Ok(pick) = rx.try_recv() {
                self.pending_output_pick = None;
                if let Some(path) = pick {
                    self.change_output_dir(Some(&path));
                }
            }
        }
        if let Some(ref rx) = self.pending_template_pick {
            if let Ok(pick) = rx.try_recv() {
                if let Some(path) = pick {
                    self.state.normalization_template = Some(path);
                }
                self.pending_template_pick = None;
            }
        }
        if let Some(ref rx) = self.pending_template_t2_pick {
            if let Ok(pick) = rx.try_recv() {
                if let Some(path) = pick {
                    self.state.normalization_template_t2 = Some(path);
                }
                self.pending_template_t2_pick = None;
            }
        }
        if let Some(ref rx) = self.pending_manifest_pick {
            if let Ok(pick) = rx.try_recv() {
                if let Some(path) = pick {
                    self.state.scene.manifest_path = path;
                    self.state.scene.manifest_path_is_manual = true;
                }
                self.pending_manifest_pick = None;
            }
        }
    }

    /// Put keyboard focus on a real control so a still capture can show the
    /// focus indicator.
    ///
    /// Injecting a Tab event here does not work: egui resolves focus during
    /// `begin_pass`, before `ui()` runs, and rebuilds `events` from raw input
    /// each frame, so an event pushed from here is simply dropped. Requesting
    /// focus by widget id is the supported route.
    fn screenshot_focus_hook(&mut self, ctx: &egui::Context) {
        if !self.screenshot_action_focus || self.screenshot_png.is_none() {
            return;
        }
        if self.frames >= 20 {
            if let Some(id) = self.stage_panel.run_button_id {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
    }

    /// Capture the window to a PNG and quit, when `DBSLOC_APP_SCREENSHOT_PNG` is
    /// set. This exists so the interface can be reviewed as it actually renders,
    /// rather than from source; it does nothing in a normal run.
    fn screenshot_hook(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_png.clone() else {
            return;
        };
        self.frames += 1;
        ctx.request_repaint();
        if self.screenshot_action_run && self.frames == 20 {
            self.run_now(ctx);
        }
        if let Some(shot) = self.screenshot_review {
            if self.frames == 2 {
                self.setup_review_shot(shot);
            }
        }
        if self.screenshot_refine && self.frames == 2 {
            self.setup_refine_shot();
        }
        if self.screenshot_group && self.frames == 2 {
            self.show_landing = false;
            self.guided = false;
            self.focus_tab(TabKind::Group);
            if let Some(file) = std::env::var_os("DBSLOC_APP_GROUP_FILE").filter(|p| !p.is_empty())
            {
                self.group_tab.open(PathBuf::from(file), ctx);
            }
        }
        if self.screenshot_group
            && self.group_scene_since.is_none()
            && self.frames >= 3
            && std::env::var_os("DBSLOC_APP_GROUP_SCENE").is_some()
            && self.group_tab.is_open()
            && self.group_tab.is_settled()
        {
            let env = self.group_env();
            self.group_tab.start_view(&env, ctx);
            self.group_scene_since = Some(self.frames);
        }
        if self.screenshot_report && self.frames == 2 {
            let paths: Vec<PathBuf> = std::env::var_os("DBSLOC_APP_REPORT_PDF")
                .map(|v| std::env::split_paths(&v).collect())
                .unwrap_or_default();
            self.show_landing = false;
            self.guided = false;
            self.open_report_dialog(paths, ctx);
        }
        if self.screenshot_stimulation && self.frames == 2 {
            self.stimulation_tab
                .set_preview_default(std::env::var_os("DBSLOC_APP_STIMULATION_PREVIEW").is_some());
            self.start_stimulation(ctx);
        }
        if self.screenshot_action_popout || self.screenshot_viewer_docked {
            if self.frames == 2 {
                self.focus_tab(TabKind::Scene);
            }
            if self.popout_since.is_none() && self.scene_panel.has_scene() {
                if self.screenshot_action_popout {
                    self.scene_panel.pop_out();
                }
                self.popout_since = Some(self.frames);
            }
        }
        let timed_out = (self.screenshot_action_popout || self.screenshot_viewer_docked)
            && self.popout_since.is_none()
            && self.frames >= POPOUT_SCENE_TIMEOUT_FRAMES;
        if timed_out && self.frames == POPOUT_SCENE_TIMEOUT_FRAMES {
            eprintln!("dbs-localizer: the scene did not load in time; capturing the main window");
        }
        if self.screenshot_review.is_some()
            && self.review_ready_since.is_none()
            && self.review_panel.volumes_ready()
        {
            self.review_ready_since = Some(self.frames);
        }
        let review_settled = match self.screenshot_review {
            None => true,
            Some(_) => {
                self.review_ready_since
                    .is_some_and(|f| self.frames >= f + REVIEW_READY_SETTLE_FRAMES)
                    || self.frames >= REVIEW_READY_TIMEOUT_FRAMES
            }
        };
        if self.screenshot_review.is_some()
            && self.frames == REVIEW_READY_TIMEOUT_FRAMES
            && self.review_ready_since.is_none()
        {
            eprintln!("dbs-localizer: the review did not load in time; capturing anyway");
        }
        let run_settled = if self.screenshot_tab_popout.is_some() {
            self.popout_since.is_some_and(|f| self.frames >= f + 60)
        } else if self.screenshot_action_popout || self.screenshot_viewer_docked {
            timed_out || self.popout_since.is_some_and(|f| self.frames >= f + 60)
        } else {
            !self.screenshot_action_run || (self.frames > 40 && !self.stage_panel.running)
        };
        if self.screenshot_stimulation
            && self.stimulation_scene_since.is_none()
            && self.frames >= 3
            && std::env::var_os("DBSLOC_APP_STIMULATION_SCENE").is_some()
            && self.stimulation_tab.is_open()
            && self.stimulation_tab.is_settled()
        {
            self.focus_tab(TabKind::Scene);
            if let Err(e) = self
                .scene_panel
                .load_default_manifest(&mut self.state, ctx, true)
            {
                eprintln!("dbs-localizer: {e}");
            }
            self.stimulation_scene_since = Some(self.frames);
        }
        let scene_shot_pending = self
            .stimulation_scene_since
            .is_some_and(|f| !self.scene_panel.has_scene() || self.frames < f + 120);
        let group_scene_wanted =
            self.screenshot_group && std::env::var_os("DBSLOC_APP_GROUP_SCENE").is_some();
        let group_settled = !self.screenshot_group
            || self.frames >= STIMULATION_SETTLE_TIMEOUT_FRAMES
            || (self.group_tab.is_settled()
                && !self.group_tab.is_opening()
                && (!group_scene_wanted
                    || (self.scene_panel.has_group_scene()
                        && self.group_look_pending.is_none()
                        && self
                            .group_scene_since
                            .is_some_and(|f| self.frames >= f + 300))));
        let stimulation_settled = !self.screenshot_stimulation
            || self.frames >= STIMULATION_SETTLE_TIMEOUT_FRAMES
            || (!scene_shot_pending && self.stimulation_tab.is_settled());
        let report_settled = (!self.screenshot_report
            || self.frames >= STIMULATION_SETTLE_TIMEOUT_FRAMES
            || self.report_import.is_settled())
            && (!self.screenshot_leaddbs
                || self.frames >= STIMULATION_SETTLE_TIMEOUT_FRAMES
                || self.leaddbs_import.is_settled());
        if let Some(tab) = self.screenshot_tab_popout {
            if self.popout_since.is_none()
                && self.frames >= 60
                && report_settled
                && review_settled
                && stimulation_settled
                && group_settled
            {
                match tab {
                    TabKind::Review => self.review_panel.pop_out(),
                    TabKind::Refine => self.refine_tab.pop_out(),
                    TabKind::Stimulation => self.stimulation_tab.pop_out(),
                    TabKind::Group => self.group_tab.pop_out(),
                    _ => {}
                }
                self.popout_since = Some(self.frames);
            }
        }
        if self.frames >= 60
            && report_settled
            && run_settled
            && review_settled
            && stimulation_settled
            && group_settled
            && !self.screenshot_requested
        {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        if !self.screenshot_requested {
            return;
        }
        if self
            .screenshot_close_at
            .is_some_and(|at| Instant::now() >= at)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.screenshot_popout_pending {
            self.screenshot_popout_pending = false;
            let popout_path = popout_screenshot_path(&path);
            let shot = crate::panels::open_popout_title(ctx)
                .ok_or_else(|| "no separate window is open".to_owned())
                .and_then(|title| {
                    crate::win32::capture_window(&title)
                        .ok_or_else(|| format!("could not capture the window \"{title}\""))
                });
            match shot {
                Ok((width, height, rgba)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    match dbsloc_viz::save_color_image(&img, &popout_path) {
                        Ok(()) => eprintln!("dbs-localizer: wrote {}", popout_path.display()),
                        Err(e) => {
                            eprintln!("dbs-localizer: {e}");
                            crate::app_log::error(&e.to_string());
                        }
                    }
                }
                Err(e) => eprintln!("dbs-localizer: {e}"),
            }
            let hold = if self.screenshot_action_popout {
                POPOUT_HOLD
            } else {
                Duration::ZERO
            };
            self.screenshot_close_at = Some(Instant::now() + hold);
            return;
        }
        if self.screenshot_close_at.is_some() {
            return;
        }
        let mut image = None;
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Screenshot { image: img, .. } = ev {
                    image = Some(Arc::clone(img));
                }
            }
        });
        if let Some(img) = image {
            match dbsloc_viz::save_color_image(&img, &path) {
                Ok(()) => eprintln!("dbs-localizer: wrote {}", path.display()),
                Err(e) => {
                    eprintln!("dbs-localizer: {e}");
                    crate::app_log::error(&e.to_string());
                }
            }
            if self.popout_since.is_some() && !self.screenshot_viewer_docked {
                self.screenshot_popout_pending = true;
            } else {
                self.screenshot_close_at = Some(Instant::now());
            }
        }
    }

    fn setup_refine_shot(&mut self) {
        let Some(request) = std::env::var_os("DBSLOC_APP_REFINE_REQUEST")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
        else {
            eprintln!("dbs-localizer: DBSLOC_APP_REFINE_REQUEST is not set");
            return;
        };
        let panel = match dbsloc_viz::refine_app::RefinePanel::from_request_path(&request) {
            Ok(panel) => panel,
            Err(e) => {
                eprintln!("dbs-localizer: {e}");
                crate::app_log::error(&e.to_string());
                return;
            }
        };
        let owner = RefineOwner {
            bids_dir: PathBuf::from(&self.state.bids_dir),
            subject: panel.request().subj_id.clone(),
        };
        self.refine_tab.begin(panel, owner);
        self.show_landing = false;
        self.focus_tab(TabKind::Refine);
    }

    fn setup_review_shot(&mut self, shot: ReviewShot) {
        let Some(manifest) = std::env::var_os("DBSLOC_APP_REVIEW_MANIFEST")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
        else {
            eprintln!("dbs-localizer: DBSLOC_APP_REVIEW_MANIFEST is not set");
            return;
        };
        let request = match crate::review::request_from_manifest(&manifest) {
            Ok(request) => request,
            Err(e) => {
                eprintln!("dbs-localizer: {e}");
                crate::app_log::error(&e.to_string());
                return;
            }
        };
        let stage = request.stage;
        let owner = ReviewOwner {
            bids_dir: PathBuf::from(&self.state.bids_dir),
            subject: request.subj_id.clone(),
        };
        let result = match shot {
            ReviewShot::Waiting => {
                let (verdict, _rx) = mpsc::channel();
                self.review_panel
                    .begin(crate::review::BridgeMsg { request, verdict }, owner)
            }
            ReviewShot::Docked | ReviewShot::Guided => {
                self.review_panel.begin_after_run(request, owner)
            }
        };
        if let Err(e) = result {
            eprintln!("dbs-localizer: {e}");
            crate::app_log::error(&e.to_string());
            return;
        }
        self.show_landing = false;
        if shot == ReviewShot::Guided {
            self.guided = true;
            self.guided_step = match stage {
                Stage::Normalize => GuidedStep::Normalize,
                _ => GuidedStep::Coregister,
            };
        } else {
            self.focus_tab(TabKind::Review);
        }
    }

    /// Closing the window during a run would abandon a multi-hour ANTs job and
    /// leave partial output behind, so ask first.
    fn guard_close_during_run(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) {
            if quit_needs_confirm(
                self.stage_panel.running,
                self.refine_tab.is_dirty(),
                self.importing(),
                self.quit_approved,
            ) {
                self.confirm_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                self.persist_state_if_changed(true);
                return;
            }
        }
        if !self.confirm_quit {
            return;
        }
        if !self.stage_panel.running {
            if self.importing() {
                self.quit_waits_for_import = true;
                self.import_quit_dialog(ctx);
            } else if std::mem::take(&mut self.quit_waits_for_import) {
                self.quit_cancels_import = false;
                if !self.refine_tab.is_dirty() {
                    self.confirm_quit = false;
                    self.quit_approved = true;
                    self.persist_state_if_changed(true);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else if self.refine_tab.is_dirty() {
                self.refine_quit_dialog(ctx);
            } else {
                self.confirm_quit = false;
            }
            return;
        }
        let mut quit = false;
        let mut stay = false;
        egui::Window::new("A run is still in progress")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "The pipeline is at {}.",
                    self.progress.progress_text()
                ));
                ui.label(
                    egui::RichText::new(
                        "Quitting now abandons the step that is running and can leave \
                         partial output in the subject folder.",
                    )
                    .small(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Keep running")
                        .on_hover_text("Return to the application and leave the run alone")
                        .clicked()
                    {
                        self.quit_after_run = false;
                        stay = true;
                    }
                    if ui
                        .button("Stop the run, then quit")
                        .on_hover_text("Stop the run and close the application once it has stopped")
                        .clicked()
                    {
                        self.pipeline_cancel.store(true, Ordering::SeqCst);
                        self.stage_panel.stopping = true;
                        self.quit_after_run = true;
                        stay = true;
                    }
                    if ui
                        .button(egui::RichText::new("Quit now").strong())
                        .on_hover_text(
                            "End the running step, wait a few seconds for it to stop, and close",
                        )
                        .clicked()
                    {
                        quit = true;
                    }
                });
            });
        if quit {
            self.confirm_quit = false;
            self.stop_runner_before_quit();
            self.quit_approved = true;
            self.persist_state_if_changed(true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if stay {
            self.confirm_quit = false;
        }
    }

    fn stop_runner_before_quit(&mut self) {
        self.pipeline_cancel.store(true, Ordering::SeqCst);
        self.review_panel.end_run();
        self.review_rx = None;
        let Some(runner) = self._runner_thread.as_ref() else {
            return;
        };
        let deadline = Instant::now() + QUIT_RUNNER_WAIT;
        while !runner.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        if !runner.is_finished() {
            crate::app_log::warn("quit while the pipeline was still stopping");
        }
    }

    fn quit_once_the_run_has_stopped(&mut self, ctx: &egui::Context) {
        if !self.quit_after_run || self.stage_panel.running {
            return;
        }
        self.quit_after_run = false;
        if self.refine_tab.is_dirty() {
            self.confirm_quit = true;
            return;
        }
        self.quit_approved = true;
        self.persist_state_if_changed(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn import_ended(&mut self, end: crate::import_wizard::ImportEnd) {
        if !self.quit_waits_for_import || end.clean {
            return;
        }
        self.quit_waits_for_import = false;
        self.quit_cancels_import = false;
        self.confirm_quit = false;
        crate::app_log::error(&format!(
            "quit set aside: the image import did not end cleanly: {}",
            end.problem.as_deref().unwrap_or("unknown problem")
        ));
    }

    fn import_quit_dialog(&mut self, ctx: &egui::Context) {
        let mut quit = false;
        let mut stay = false;
        let mut cancel = false;
        let cancelling = self.quit_cancels_import;
        egui::Window::new("The image import is still writing")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(if cancelling {
                    "The import is being cancelled; the subject it was writing is rolled back. \
                     The application closes when that has finished."
                } else {
                    "The import is still writing images into the dataset. Quitting now can leave \
                     the raw data of the subject half-written. The application closes by itself \
                     when the import has finished."
                });
                ui.add_space(em(ui) * 0.4);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!cancelling, egui::Button::new("Keep importing"))
                        .on_hover_text("Return to the application and let the import finish")
                        .on_disabled_hover_text("The import is being cancelled")
                        .clicked()
                    {
                        stay = true;
                    }
                    if ui
                        .add_enabled(
                            !cancelling,
                            egui::Button::new("Cancel the import, then quit"),
                        )
                        .on_hover_text(
                            "Stop the import, remove what it wrote, and close the application",
                        )
                        .on_disabled_hover_text("The import is being cancelled")
                        .clicked()
                    {
                        cancel = true;
                    }
                    if ui
                        .button(egui::RichText::new("Quit now").strong())
                        .on_hover_text("Close the application while the import is still writing")
                        .clicked()
                    {
                        quit = true;
                    }
                });
            });
        if quit {
            self.confirm_quit = false;
            self.quit_waits_for_import = false;
            self.quit_cancels_import = false;
            self.quit_approved = true;
            self.persist_state_if_changed(true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if stay {
            self.confirm_quit = false;
            self.quit_waits_for_import = false;
            self.quit_cancels_import = false;
        } else if cancel {
            if let Some(w) = self.import.as_mut() {
                w.cancel_job();
            }
            self.quit_cancels_import = true;
        }
    }

    fn refine_quit_dialog(&mut self, ctx: &egui::Context) {
        let mut quit = false;
        let mut stay = false;
        egui::Window::new("Unsaved lead corrections")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Unsaved lead corrections will be lost. Quit anyway?");
                ui.add_space(em(ui) * 0.4);
                ui.horizontal(|ui| {
                    if ui
                        .button("Keep editing")
                        .on_hover_text("Return to Refine localization and keep the corrections")
                        .clicked()
                    {
                        stay = true;
                    }
                    if ui
                        .button(egui::RichText::new("Quit").strong())
                        .on_hover_text("Close the application and lose the unsaved corrections")
                        .clicked()
                    {
                        quit = true;
                    }
                });
            });
        if quit {
            self.confirm_quit = false;
            self.quit_approved = true;
            self.persist_state_if_changed(true);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if stay {
            self.confirm_quit = false;
        }
    }

    fn run_wait(&self) -> Option<&'static str> {
        run_wait_for(RunWaitState {
            review_preparing: self.review_prepare.is_some(),
            review_finishing: self.review_finishing.load(Ordering::SeqCst) > 0,
            refine_saving: self.refine_busy.load(Ordering::SeqCst) > 0,
            refine_preparing: refine_prepare_blocks(
                self.refine_prepare.is_some(),
                self.refine_prepare_follow,
            ),
            refine_dirty: self.refine_tab.is_dirty(),
            importing: self.importing(),
        })
    }

    fn start_review_prepare(&mut self, stage: Stage, ctx: &egui::Context) {
        if self.stage_panel.running {
            self.push_error("Registration review", RUN_ALREADY_ACTIVE);
            return;
        }
        if self.review_prepare.is_some() {
            self.push_error("Registration review", RUN_WAIT_PREPARING);
            return;
        }
        if self.refine_prepare.is_some() {
            self.push_error("Registration review", RUN_WAIT_REFINE_PREPARING);
            return;
        }
        if self.refine_busy.load(Ordering::SeqCst) > 0 {
            self.push_error("Registration review", RUN_WAIT_REFINE_SAVING);
            return;
        }
        if self.importing() {
            self.push_error("Registration review", RUN_WAIT_IMPORT);
            return;
        }
        let Some(subj) = self.state.selected_subject.clone() else {
            return;
        };
        let Some(req) = self.stage_panel.build_request(&self.state, &[stage]) else {
            return;
        };
        let subject = subj.strip_prefix("sub-").unwrap_or(&subj).to_owned();
        let template = self
            .state
            .normalization_template
            .as_deref()
            .map(PathBuf::from);
        self.prepare_owner = Some(ReviewOwner {
            bids_dir: PathBuf::from(&self.state.bids_dir),
            subject: subject.clone(),
        });
        let opts = pipeline_run_options(
            req,
            PathBuf::from(&self.state.bids_dir),
            self.state.output_dir(),
            subject,
            template,
            false,
            has_both_postop(self.readiness.get()),
        );
        let (tx, rx) = mpsc::channel::<PreparedReview>();
        self.review_prepare = Some(rx);
        self.stage_panel.review_preparing = Some(stage);
        self.review_panel.set_notice_error(None);
        self.log_panel.push(format!(
            "◔ {}: preparing the registration images",
            stage.label()
        ));
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = prepare_review_job(&opts, stage);
            let _ = tx.send((stage, result));
            ctx.request_repaint();
        });
    }

    fn poll_review_prepare(&mut self) {
        let Some(rx) = &self.review_prepare else {
            return;
        };
        let msg = match rx.try_recv() {
            Ok(msg) => Some(msg),
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.review_prepare = None;
        let preparing = self.stage_panel.review_preparing.take();
        let Some((stage, result)) = msg else {
            if let Some(stage) = preparing {
                self.review_failed(stage, "the preparation stopped unexpectedly".to_owned());
            }
            return;
        };
        match result {
            Ok(request) => {
                if self.stage_panel.running {
                    self.log_panel.push(format!(
                        "{}: a run started, the registration review was not opened",
                        stage.label()
                    ));
                    return;
                }
                let Some(owner) = self.prepare_owner.clone() else {
                    self.review_failed(stage, "the subject of the review is unknown".to_owned());
                    return;
                };
                match self.review_panel.begin_after_run(request, owner) {
                    Ok(()) => {
                        self.log_panel
                            .push(format!("◔ {}: registration review ready", stage.label()));
                        if !self.guided {
                            self.focus_tab(TabKind::Review);
                        }
                    }
                    Err(e) => self.review_failed(stage, e),
                }
            }
            Err(e) => self.review_failed(stage, e),
        }
    }

    fn review_failed(&mut self, stage: Stage, error: String) {
        let body = crate::panels::humanize_message(&error);
        if let Some(b) = self.batch.as_mut() {
            if b.queue.reviewing().is_some_and(|r| r.stage == stage) {
                b.queue.skip_review(Some(&format!(
                    "the registration review could not be prepared: {body}"
                )));
            }
        }
        let text = format!(
            "{}: the registration review could not be prepared: {body}",
            stage.label()
        );
        if self.guided {
            self.push_error("Registration review", text);
            return;
        }
        self.log_panel.push_error(text.clone());
        self.review_panel.set_notice_error(Some(text));
        self.focus_tab(TabKind::Review);
    }

    fn start_refine_prepare(&mut self, ctx: &egui::Context) {
        self.begin_refine_prepare(ctx, false);
    }

    fn begin_refine_prepare(&mut self, ctx: &egui::Context, follow: bool) {
        let refuse = |app: &mut Self, reason: &str| {
            if follow {
                let target = crate::follow::Target::new(
                    PathBuf::from(&app.state.bids_dir),
                    app.state.selected_subject.as_deref(),
                );
                app.refine_tab.show_empty(target, Some(reason.to_owned()));
            } else {
                app.push_error("Refine localization", reason);
            }
        };
        let refusal = if self.stage_panel.running {
            Some("A run is in progress. Wait until it has finished or stop it first.")
        } else if self.review_prepare.is_some() {
            Some(RUN_WAIT_PREPARING)
        } else if self.review_finishing.load(Ordering::SeqCst) > 0 {
            Some(RUN_WAIT_FINISHING)
        } else if self.refine_busy.load(Ordering::SeqCst) > 0 {
            Some(RUN_WAIT_REFINE_SAVING)
        } else if self.refine_prepare.is_some() {
            Some(RUN_WAIT_REFINE_PREPARING)
        } else if self.refine_tab.is_dirty() {
            Some("Save or discard the corrections in the open Refine localization tab first.")
        } else if self.importing() {
            Some(RUN_WAIT_IMPORT)
        } else {
            None
        };
        if let Some(reason) = refusal {
            refuse(self, reason);
            return;
        }
        let Some(subj) = self.state.selected_subject.clone() else {
            refuse(self, "Select a subject first.");
            return;
        };
        let Some(req) = self
            .stage_panel
            .build_request(&self.state, &[Stage::Reconstruct])
        else {
            refuse(
                self,
                "The settings of the Reconstruct step could not be read for this subject.",
            );
            return;
        };
        let subject = subj.strip_prefix("sub-").unwrap_or(&subj).to_owned();
        if follow {
            self.refine_opts = None;
            self.refine_tab.show_opening(crate::follow::Target::new(
                PathBuf::from(&self.state.bids_dir),
                Some(&subject),
            ));
        }
        self.refine_prepare_follow = follow;
        self.note_jobs();
        self.refine_prepare_epoch = self.job_epoch;
        self.refine_prepare_owner = Some(RefineOwner {
            bids_dir: PathBuf::from(&self.state.bids_dir),
            subject: subject.clone(),
        });
        let template = self
            .state
            .normalization_template
            .as_deref()
            .map(PathBuf::from);
        let opts = pipeline_run_options(
            req,
            PathBuf::from(&self.state.bids_dir),
            self.state.output_dir(),
            subject,
            template,
            false,
            has_both_postop(self.readiness.get()),
        );
        let (tx, rx) = mpsc::channel::<PreparedRefine>();
        self.refine_prepare = Some(rx);
        self.log_panel
            .push("◔ Reconstruct: opening the lead on the post-op image");
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(prepare_refine_job(opts));
            ctx.request_repaint();
        });
    }

    fn poll_refine_prepare(&mut self) {
        let Some(rx) = &self.refine_prepare else {
            return;
        };
        let msg = match rx.try_recv() {
            Ok(msg) => msg,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("the preparation stopped unexpectedly".to_owned())
            }
        };
        self.refine_prepare = None;
        let expected = self.refine_prepare_owner.take();
        let follow = std::mem::take(&mut self.refine_prepare_follow);
        let awaited = expected.as_ref().is_some_and(|o| self.refine_tab.awaits(o));
        let follow_target = expected
            .as_ref()
            .map(|o| crate::follow::Target::new(o.bids_dir.clone(), Some(&o.subject)));
        let current = expected.as_ref().is_some_and(|o| {
            refine_owner_matches(
                o,
                &self.state.bids_dir,
                self.state.selected_subject.as_deref(),
            )
        });
        let (panel, owner, opts) = match msg {
            Ok(prepared) => prepared,
            Err(e) => {
                match follow_target.filter(|_| follow && awaited) {
                    Some(target) => self
                        .refine_tab
                        .show_empty(target, Some(format!("The lead could not be opened: {e}"))),
                    None if follow => {}
                    None => self.push_error(
                        "Refine localization",
                        format!("the lead could not be opened: {e}"),
                    ),
                }
                return;
            }
        };
        let action = prepared_refine_action(PreparedRefineState {
            follow,
            current,
            awaited,
            running: self.stage_panel.running,
            importing: self.importing(),
            job_since_start: self.job_epoch != self.refine_prepare_epoch,
        });
        match action {
            PreparedRefineAction::Drop => {
                self.log_panel
                    .push("dbsloc: refine: subject changed while preparing, discarded");
            }
            PreparedRefineAction::DropAndRetry => {
                self.log_panel
                    .push("dbsloc: refine: subject changed while preparing, discarded");
                if let Some(target) = follow_target.filter(|_| awaited) {
                    self.refine_tab.show_retry(target, None);
                }
            }
            PreparedRefineAction::RefuseRun => self.push_error(
                "Refine localization",
                "A run started while the lead was opening, so Refine localization was not opened. Open it again when the run has finished.",
            ),
            PreparedRefineAction::RetryAfterJob => {
                if let Some(target) = follow_target {
                    self.refine_tab.show_retry(
                        target,
                        Some(
                            "A run or an image import started while the lead was opening. The lead opens when it has finished."
                                .to_owned(),
                        ),
                    );
                }
            }
            PreparedRefineAction::OpenQuietly => {
                let subject = owner.subject.clone();
                self.refine_tab.begin(panel, owner);
                self.refine_opts = Some(opts);
                self.log_panel.push(format!(
                    "◔ Reconstruct: Refine localization shows sub-{subject}"
                ));
            }
            PreparedRefineAction::Open => {
                self.refine_tab.begin(panel, owner);
                self.refine_opts = Some(opts);
                self.log_panel
                    .push("◔ Reconstruct: Refine localization ready");
                self.show_landing = false;
                self.guided = false;
                self.focus_tab(TabKind::Refine);
            }
        }
    }

    fn start_refine_save(
        &mut self,
        edits: Vec<dbsloc_viz::refine_app::SideEdit>,
        owner: RefineOwner,
        ctx: &egui::Context,
    ) {
        if self.refine_save.is_some() || self.refine_tab.is_saving() {
            return;
        }
        if self.stage_panel.running {
            self.push_error(
                "Refine localization",
                "A run is in progress. Save the correction when it has finished.",
            );
            return;
        }
        if self.importing() {
            self.push_error("Refine localization", RUN_WAIT_IMPORT);
            return;
        }
        let Some(opts) = self.refine_opts.clone() else {
            self.push_error(
                "Refine localization",
                "The correction could not be saved: the subject settings were not captured when the lead was opened.",
            );
            return;
        };
        let Some(space) = self.refine_tab.edit_space() else {
            self.push_error(
                "Refine localization",
                "The correction could not be saved: no lead is open in Refine localization.",
            );
            return;
        };
        let request = dbsloc_pipeline::manual_correction::CorrectionRequest {
            space,
            source: "gui".to_owned(),
            sides: pipeline_edits(&edits),
        };
        self.refine_cancel.store(false, Ordering::SeqCst);
        let cancel = Arc::clone(&self.refine_cancel);
        let busy = BusyGuard::new(&self.refine_busy);
        let (tx, rx) = mpsc::channel::<RefineSaveMsg>();
        self.refine_save = Some(rx);
        self.refine_tab.begin_save();
        self.log_panel.push(format!(
            "◔ Reconstruct: saving the corrected lead of sub-{}",
            owner.subject
        ));
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = resolve_run_subject(&opts)
                .map_err(|e| e.to_string())
                .and_then(|sub| {
                    let mut log = |line: String| {
                        let _ = tx.send(RefineSaveMsg::Log(line));
                        ctx.request_repaint();
                    };
                    dbsloc_pipeline::apply_manual_correction(
                        &sub,
                        &opts,
                        &request,
                        Some(cancel.as_ref()),
                        &mut log,
                    )
                    .map_err(|e| e.to_string())
                });
            let _ = tx.send(RefineSaveMsg::Done(result));
            drop(busy);
            ctx.request_repaint();
        });
    }

    fn poll_refine_save(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.refine_save else {
            return;
        };
        let mut done = None;
        let mut lines = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(RefineSaveMsg::Log(line)) => lines.push(line),
                Ok(RefineSaveMsg::Done(result)) => {
                    done = Some(result);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = Some(Err("the save stopped unexpectedly".to_owned()));
                    break;
                }
            }
        }
        for line in lines {
            self.log_panel.push(line);
        }
        let Some(result) = done else {
            return;
        };
        self.refine_save = None;
        let Some(owner) = self.refine_tab.owner().cloned() else {
            self.refine_tab.finish_save(false, None);
            return;
        };
        match result {
            Ok(report) => {
                for line in report_lines(&report) {
                    self.log_panel.push(line);
                }
                let mut notice = "Saved the corrected lead.".to_owned();
                if !report.invalidated.is_empty() {
                    notice.push_str(&format!(
                        " Rerun {} to bring them up to date.",
                        report.invalidated.join(", ")
                    ));
                }
                self.refine_tab.finish_save(true, Some(notice));
                self.readiness.invalidate();
                let folder = self
                    .state
                    .derivatives_root_for(&owner.bids_dir)
                    .join(format!("sub-{}", owner.subject));
                self.scene_panel.reload_loaded_under(&folder, ctx);
            }
            Err(_) if self.refine_cancel.load(Ordering::SeqCst) => {
                self.refine_tab.finish_save(false, None);
                self.log_panel.push("dbsloc: manual correction cancelled");
            }
            Err(e) => {
                self.refine_tab.finish_save(false, None);
                self.push_error(
                    "Refine localization",
                    format!("the correction was not saved: {e}"),
                );
            }
        }
    }

    fn start_stimulation(&mut self, ctx: &egui::Context) {
        self.start_stimulation_focused(ctx, None);
    }

    fn start_stimulation_focused(
        &mut self,
        ctx: &egui::Context,
        focus: Option<crate::stimulation_panel::StimFocus>,
    ) {
        if self.stimulation_tab.is_preparing() {
            return;
        }
        if self.stimulation_tab.is_dirty() {
            self.push_error(
                "Stimulation",
                "Save or discard the settings in the open stimulation editor first.",
            );
            self.focus_tab(TabKind::Stimulation);
            return;
        }
        let mut subject_changed = false;
        if let Some(f) = &focus {
            if self.state.selected_subject.as_deref() != Some(f.subject.as_str()) {
                if self.stage_panel.running {
                    self.push_error("Cannot change the subject now", SUBJECT_LOCKED);
                    return;
                }
                self.state.selected_subject = Some(f.subject.clone());
                let bids_dir = self.state.bids_dir.clone();
                remember_subject_in_state(&mut self.state, &bids_dir, &f.subject);
                self.progress = RunProgress::default();
                subject_changed = true;
            }
        }
        let Some(subj) = self.state.selected_subject.clone() else {
            self.push_error("Stimulation", "Select a subject first.");
            return;
        };
        let Some(req) = self.stage_panel.build_request(&self.state, &[Stage::Vat]) else {
            self.push_error(
                "Stimulation",
                "The settings of the volume of activated tissue step could not be read for this subject.",
            );
            return;
        };
        let subject = subj.strip_prefix("sub-").unwrap_or(&subj).to_owned();
        let template = self
            .state
            .normalization_template
            .as_deref()
            .map(PathBuf::from);
        let opts = pipeline_run_options(
            req,
            PathBuf::from(&self.state.bids_dir),
            self.state.output_dir(),
            subject,
            template,
            false,
            !subject_changed && has_both_postop(self.readiness.get()),
        );
        self.stimulation_tab.open(opts, focus, ctx);
        self.show_landing = false;
        self.guided = false;
        self.focus_tab(TabKind::Stimulation);
    }

    fn apply_stimulation_output(
        &mut self,
        out: crate::stimulation_panel::StimOutput,
        ctx: &egui::Context,
    ) {
        for (title, body) in out.errors {
            self.push_error(title, body);
        }
        for line in out.log {
            self.log_panel.push(line);
        }
        if out.focus {
            self.focus_tab(TabKind::Stimulation);
        }
        if out.saved {
            self.group_tab.rescan();
        }
        if out.open_connectomes {
            self.connectome_panel.open();
        }
        if let Some(spec) = out.remember_connectome {
            self.state.stim_connectome = Some(spec);
        }
        if let Some(layer) = out.fibre_layer {
            self.scene_panel.upsert_fibre_layer(layer);
            self.focus_tab(TabKind::Scene);
        }
        if let Some(label) = out.compute {
            if let Some(bound) = self.stimulation_foreign_subject() {
                let selection = if self.state.selected_subject.is_some() {
                    "another subject or dataset is selected"
                } else {
                    "no subject is selected"
                };
                self.push_error(
                    "Cannot compute the VTA",
                    format!(
                        "The stimulation editor belongs to sub-{bound}, but {selection}. Select \
                         sub-{bound} again, or close the editor and open it for the subject you \
                         want. The settings are saved for sub-{bound}; nothing was computed."
                    ),
                );
                self.stimulation_tab.run_ended();
                return;
            }
            let Some(mut req) = self.stage_panel.build_request(&self.state, &[Stage::Vat]) else {
                self.push_error(
                    "Stimulation",
                    "The settings of the volume of activated tissue step could not be read.",
                );
                self.stimulation_tab.run_ended();
                return;
            };
            req.vat_labels = vec![label];
            req.overwrite_approved = true;
            match self.try_spawn_run(req, ctx) {
                Ok(()) => self.focus_tab(TabKind::Output),
                Err(reason) => {
                    self.push_error("Cannot compute the VTA", reason);
                    self.stimulation_tab.run_ended();
                }
            }
        }
    }

    fn stimulation_foreign_subject(&self) -> Option<String> {
        let (dir, subject) = self.stimulation_tab.bound_subject()?;
        (!crate::stimulation_panel::stim_target_matches(
            dir,
            subject,
            Path::new(&self.state.bids_dir),
            self.state.selected_subject.as_deref(),
        ))
        .then(|| subject.to_owned())
    }

    fn stim_env(&self) -> crate::stimulation_panel::StimEnv {
        crate::stimulation_panel::StimEnv {
            run_active: self.stage_panel.running || self.batch.is_some(),
            batch_active: self.batch.is_some(),
            run_wait: self.run_wait(),
            foreign_subject: self.stimulation_foreign_subject().is_some(),
            subject_selected: self.state.selected_subject.is_some(),
        }
    }

    fn selection_target(&self) -> crate::follow::Target {
        crate::follow::Target::new(
            PathBuf::from(&self.state.bids_dir),
            self.state.selected_subject.as_deref(),
        )
    }

    fn note_jobs(&mut self) {
        let (importing, epoch) =
            importing_edge(self.job_epoch_importing, self.importing(), self.job_epoch);
        self.job_epoch_importing = importing;
        self.job_epoch = epoch;
    }

    fn follow_selection(&mut self, ctx: &egui::Context) {
        use crate::follow::Follow;
        if self.dock.find_tab(&TabKind::Stimulation).is_none() {
            self.stimulation_tab.forget_target();
        }
        if self.dock.find_tab(&TabKind::Refine).is_none() {
            self.refine_tab.forget_target();
        }
        let generation = self.readiness.generation();
        if self.follow_readiness_seen != generation {
            self.follow_readiness_seen = generation;
            self.stimulation_tab.mark_stale();
            self.refine_tab.mark_stale();
        }
        let selected = self.selection_target();
        let run_active = self.stage_panel.running || self.batch.is_some();
        let ready = self.readiness.is_current_for(&self.state);
        let stage_panel = &self.stage_panel;
        let state = &self.state;
        let readiness = &self.readiness;
        let stim =
            self.stimulation_tab
                .follow_selection(&selected, run_active, ready, ctx, |target| {
                    stimulation_follow_options(stage_panel, state, readiness, target)
                });
        if stim == Follow::Soon {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.screenshot_refine {
            return;
        }
        let refine_busy = refine_follow_busy(RefineFollowJobs {
            run_active,
            preparing: self.refine_prepare.is_some(),
            saving: self.refine_save.is_some() || self.refine_busy.load(Ordering::SeqCst) > 0,
            review_preparing: self.review_prepare.is_some(),
            review_finishing: self.review_finishing.load(Ordering::SeqCst) > 0,
            importing: self.importing(),
        });
        match self
            .refine_tab
            .follow_selection(&selected, refine_busy, ready)
        {
            Follow::Load => self.begin_refine_prepare(ctx, true),
            Follow::Clear => self.refine_opts = None,
            Follow::Soon => ctx.request_repaint_after(Duration::from_millis(250)),
            _ => {}
        }
    }

    fn group_env(&self) -> crate::group_panel::GroupEnv {
        let bids = self.state.bids_dir.trim();
        let dataset_root = (!bids.is_empty()).then(|| PathBuf::from(bids));
        crate::group_panel::GroupEnv {
            run_active: self.stage_panel.running,
            derivatives_root: self.state.derivatives_root(),
            dataset_root,
            selected_subject: self.state.selected_subject.clone(),
            ticked_subjects: self.batch_subjects(),
            template: self.state.normalization_template.clone(),
            atlases_dir: self.state.stage.leaddbs_atlases_dir.clone(),
            recent: self.state.recent_groups.clone(),
            connectomes_dir: self.state.stage.connectomes_dir.clone(),
            last_connectomes: self.state.group_connectomes.clone(),
        }
    }

    fn start_group_calc(&mut self, ctx: &egui::Context) {
        if self.importing() {
            self.push_error("Cannot compute the group VTAs now", RUN_WAIT_IMPORT);
            return;
        }
        let Some(req) = self.stage_panel.build_request(&self.state, &[Stage::Vat]) else {
            self.push_error(
                "Group",
                "The settings of the volume of activated tissue step could not be read.",
            );
            return;
        };
        let template = self
            .state
            .normalization_template
            .as_deref()
            .map(PathBuf::from);
        let opts = pipeline_run_options(
            req,
            PathBuf::from(&self.state.bids_dir),
            self.state.output_dir(),
            String::new(),
            template,
            false,
            false,
        );
        let out = self.group_tab.start_calc(opts, ctx);
        self.apply_group_output(out, ctx);
    }

    fn apply_group_output(&mut self, out: crate::group_panel::GroupOutput, ctx: &egui::Context) {
        for (title, body) in out.errors {
            if title == "Group 3D view" {
                self.group_layers_view_started = false;
                if !self.group_layers_pending.is_empty() {
                    self.group_layers_pending.clear();
                    self.push_error(
                        "Group layers",
                        "The group scene could not be built, so the new fibres and maps were not drawn.",
                    );
                }
            }
            self.push_error(title, body);
        }
        for line in out.log {
            self.log_panel.push(line);
        }
        if let Some(path) = out.remember {
            crate::group_panel::remember_group(&mut self.state.recent_groups, &path);
        }
        if out.focus {
            self.focus_tab(TabKind::Group);
        }
        if let Some(look) = out.look {
            self.group_look_pending = Some(look);
        }
        if let Some((kind, spec)) = out.remember_connectome {
            self.state.group_connectomes.insert(kind, spec);
        }
        for id in &out.remove_layers {
            self.scene_panel.remove_fibre_layer(id);
            self.scene_panel.remove_map_layer(id);
            self.group_layers_pending.retain(|l| match l {
                crate::group_explorers::GroupLayer::Fibre(f) => &f.id != id,
                crate::group_explorers::GroupLayer::Map(m) => &m.id != id,
            });
        }
        if !out.layers.is_empty() {
            self.group_layers_pending.extend(out.layers);
            self.group_layers_view_started = false;
            self.focus_tab(TabKind::Scene);
        }
        if let Some(manifest) = out.show_scene {
            self.focus_tab(TabKind::Scene);
            self.scene_panel
                .load_manifest_file(&mut self.state, ctx, manifest);
        }
        if out.calc {
            self.start_group_calc(ctx);
        }
        if let Some(focus) = out.edit_stimulation {
            self.start_stimulation_focused(ctx, Some(focus));
        }
    }

    fn sync_group_layers(&mut self, ctx: &egui::Context) {
        if self.group_layers_pending.is_empty() {
            self.group_layers_view_started = false;
            return;
        }
        if !self.scene_panel.has_group_scene() {
            let tab_settled = self.group_tab.is_settled();
            if self.group_layers_view_started {
                if tab_settled && !self.scene_panel.is_loading() {
                    self.group_layers_view_started = false;
                    self.group_layers_pending.clear();
                    self.push_error(
                        "Group layers",
                        "The group scene could not be shown, so the new fibres and maps were not drawn.",
                    );
                }
                return;
            }
            if self.group_tab.is_open() {
                if tab_settled {
                    self.group_layers_view_started = true;
                    let env = self.group_env();
                    self.group_tab.start_view(&env, ctx);
                }
                return;
            }
            if self.group_tab.is_opening() {
                return;
            }
            self.log_panel.push(
                "group: no group is open, the layers are drawn in the current scene".to_owned(),
            );
        }
        for layer in std::mem::take(&mut self.group_layers_pending) {
            match layer {
                crate::group_explorers::GroupLayer::Fibre(f) => {
                    self.scene_panel.upsert_fibre_layer(f);
                }
                crate::group_explorers::GroupLayer::Map(m) => {
                    self.scene_panel.upsert_map_layer(m);
                }
            }
        }
        self.group_layers_view_started = false;
    }

    fn sync_group_look(&mut self) {
        let Some(look) = self.group_look_pending.as_ref() else {
            return;
        };
        if self.scene_panel.has_group_scene() && self.scene_panel.apply_group_look(look) {
            self.group_look_pending = None;
        }
    }

    fn sync_stimulation_scene(&mut self, ctx: &egui::Context) {
        let Some(key) = self.scene_panel.scene_key() else {
            return;
        };
        let (serial, surfaces) = self.stimulation_tab.scene_surfaces();
        if self.stimulation_scene == Some((serial, key)) {
            return;
        }
        if serial == 0 && self.stimulation_scene.is_none() {
            self.stimulation_scene = Some((serial, key));
            return;
        }
        self.scene_panel.set_vta_surfaces(surfaces.to_vec());
        self.stimulation_scene = Some((serial, key));
        ctx.request_repaint();
    }

    fn apply_refine_output(&mut self, out: RefineTabOutput, ctx: &egui::Context) {
        for e in out.errors {
            self.push_error("Refine localization", e);
        }
        if out.focus {
            self.focus_tab(TabKind::Refine);
        }
        if out.stop_saving {
            self.refine_cancel.store(true, Ordering::SeqCst);
        }
        if out.cancelled {
            self.refine_opts = None;
            self.log_panel
                .push("Refine localization closed without saving");
        }
        if let (Some(edits), Some(owner)) = (out.save, out.owner) {
            self.start_refine_save(edits, owner, ctx);
        }
    }

    fn finish_review_in_background(
        &mut self,
        stage: Stage,
        owner: ReviewOwner,
        ctx: &egui::Context,
    ) {
        let output_dir = self.state.output_dir_for(&owner.bids_dir);
        let opts = dbsloc_pipeline::options::RunOptions {
            output_dir,
            ..dbsloc_pipeline::options::RunOptions::new(owner.bids_dir, owner.subject)
        };
        let busy = BusyGuard::new(&self.review_finishing);
        let ctx = ctx.clone();
        thread::spawn(move || {
            finish_review_job(&opts, stage);
            drop(busy);
            ctx.request_repaint();
        });
    }

    fn poll_review_requests(&mut self) {
        let Some(rx) = &self.review_rx else {
            return;
        };
        let mut pending = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            pending.push(msg);
        }
        for msg in pending {
            let stage = msg.request.stage;
            let owner = self.run_owner.clone().unwrap_or_else(|| ReviewOwner {
                bids_dir: PathBuf::from(&self.state.bids_dir),
                subject: self
                    .state
                    .selected_subject
                    .as_deref()
                    .map(|s| s.strip_prefix("sub-").unwrap_or(s).to_owned())
                    .unwrap_or_default(),
            });
            match self.review_panel.begin(msg, owner) {
                Ok(()) => {
                    self.show_landing = false;
                    if self.guided {
                        if let Some(step) =
                            GuidedStep::ALL.iter().find(|s| s.stage() == Some(stage))
                        {
                            self.guided_step = *step;
                        }
                    }
                    self.progress.on_review_requested(stage);
                    self.log_panel.push(format!(
                        "◔ {}: waiting for the registration review",
                        stage.label()
                    ));
                    self.focus_tab(TabKind::Review);
                }
                Err(e) => self.push_error("Registration review", e),
            }
        }
    }

    fn apply_review_output(&mut self, out: ReviewOutput, ctx: &egui::Context) {
        for e in out.errors {
            self.push_error("Registration review", e);
        }
        if out.decided {
            self.readiness.invalidate();
        }
        if let Some((stage, approved)) = out.reviewed {
            self.log_panel.push(format!(
                "{} {}: registration {}",
                if approved { "✔" } else { "⚠" },
                stage.label(),
                if approved { "approved" } else { "rejected" }
            ));
            self.readiness.invalidate();
            if let (Some(b), Some(owner)) = (self.batch.as_mut(), out.reviewed_owner.as_ref()) {
                let folder = format!("sub-{}", owner.subject);
                if b.queue.review_done(&folder, stage, approved) {
                    self.log_panel.push(if approved {
                        format!("{folder}: the remaining steps are queued")
                    } else {
                        format!(
                            "{folder}: needs rework — rerun {} for this subject",
                            stage.label()
                        )
                    });
                }
            }
            match out.reviewed_owner {
                Some(owner) => self.finish_review_in_background(stage, owner, ctx),
                None => self.log_panel.push_error(format!(
                    "{}: the review clean-up was skipped, the subject is unknown",
                    stage.label()
                )),
            }
        }
        if let Some((stage, approved)) = out.answered {
            self.progress.on_review_answered(stage);
            self.log_panel.push(format!(
                "{} {}: registration {}",
                if approved { "✔" } else { "⚠" },
                stage.label(),
                if approved {
                    "approved, continuing the run"
                } else {
                    "rejected, stopping the run"
                }
            ));
        }
    }

    fn focus_tab(&mut self, tab: TabKind) {
        focus_in_dock(&mut self.dock, tab);
    }

    fn open_group_analysis(&mut self) {
        self.show_landing = false;
        self.guided = false;
        self.focus_tab(TabKind::Group);
    }

    fn refresh(&mut self) {
        self.readiness.invalidate();
        self.subject_panel.invalidate();
        self.group_tab.rescan();
    }

    fn run_now(&mut self, ctx: &egui::Context) {
        if self.show_landing {
            return;
        }
        if self.guided {
            self.run_guided_step(ctx);
            return;
        }
        if self.stage_panel.running {
            self.push_error("Cannot start the run", RUN_ALREADY_ACTIVE);
            return;
        }
        if self.batch.is_none() && self.batch_subjects().len() >= 2 {
            let stages = self.state.stage.selected_stages();
            match self.stage_panel.build_request(&self.state, &stages) {
                Some(req) => self.request_batch(req, ctx),
                None => self.push_error(
                    "Cannot start this run yet",
                    "Select at least one pipeline step.",
                ),
            }
            return;
        }
        let readiness = self.readiness.get().clone();
        let runnable = self.state.stage.selected_runnable_stages(&readiness);
        let blockers = self.state.stage.selection_blockers(&readiness, &runnable);
        if let Some(first) = blockers.first() {
            self.push_error(
                "Cannot start this run yet",
                if blockers.len() == 1 {
                    first.clone()
                } else {
                    format!(
                        "{first}\n\n… and {} more, listed in the Pipeline panel.",
                        blockers.len() - 1
                    )
                },
            );
            self.focus_tab(TabKind::Pipeline);
            return;
        }
        let Some(req) = self.stage_panel.build_request(&self.state, &runnable) else {
            return;
        };
        let overwritten = if self.state.stage.overwrite_results {
            self.state
                .stage
                .stages_that_would_be_overwritten(&readiness)
        } else {
            Vec::new()
        };
        if overwritten.is_empty() {
            self.spawn_run(req, ctx);
            self.focus_tab(TabKind::Output);
        } else {
            self.ask_overwrite(req, overwritten);
        }
    }

    /// Open `subject` of `bids_dir` from the start screen: the guide when it is
    /// switched on, the dock otherwise.
    fn open_subject(&mut self, bids_dir: &str, subject: &str) {
        let changed = self.state.bids_dir != bids_dir
            || self.state.selected_subject.as_deref() != Some(subject);
        if changed && self.stage_panel.running {
            self.push_error("Cannot change the subject now", SUBJECT_LOCKED);
            return;
        }
        self.remember_bids_dir(bids_dir);
        self.state.selected_subject = Some(subject.to_owned());
        remember_subject_in_state(&mut self.state, bids_dir, subject);
        if changed && !self.stage_panel.running {
            self.progress = RunProgress::default();
        }
        self.show_landing = false;
        self.guided = self.state.guided_default;
        self.guided_step = GuidedStep::FIRST_WORK;
    }

    fn open_report_picker(&mut self, ctx: &egui::Context) {
        if self.pending_report_pick.is_some() || self.report_import.is_open() {
            return;
        }
        if self.state.selected_subject.is_none() || self.state.bids_dir.trim().is_empty() {
            self.push_error(
                "Session report",
                "Open a subject first: the report is imported into the selected subject.",
            );
            return;
        }
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending_report_pick = Some(rx);
        thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Select one or more session report PDF files")
                .add_filter("PDF document", &["pdf"])
                .pick_files()
                .unwrap_or_default();
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
    }

    fn open_report_dialog(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        if paths.is_empty() {
            return;
        }
        let Some(selected) = self.state.selected_subject.clone() else {
            self.push_error("Session report", "Open a subject first.");
            return;
        };
        let id = selected
            .strip_prefix("sub-")
            .unwrap_or(&selected)
            .to_owned();
        let output = self.state.output_dir();
        match dbsloc_pipeline::bids::resolve_subject_in(
            Path::new(self.state.bids_dir.trim()),
            output.as_deref(),
            &id,
            true,
        ) {
            Ok(subject) => self.report_import.open(paths, subject, ctx),
            Err(e) => self.push_error("Session report", e.to_string()),
        }
    }

    fn apply_report_actions(
        &mut self,
        actions: Vec<crate::report_import::ReportImportAction>,
        ctx: &egui::Context,
    ) {
        use crate::report_import::ReportImportAction;
        for action in actions {
            match action {
                ReportImportAction::Log(lines) => {
                    for line in lines {
                        if line.starts_with("dbsloc: error:") {
                            self.log_panel.push_error(line);
                        } else {
                            self.log_panel.push(line);
                        }
                    }
                }
                ReportImportAction::Imported { labels, subject } => {
                    crate::report_import::invalidate_reports_cache(ctx);
                    self.refresh();
                    let current = self
                        .state
                        .selected_subject
                        .as_deref()
                        .map(|s| s.strip_prefix("sub-").unwrap_or(s).to_owned());
                    let imported = subject.strip_prefix("sub-").unwrap_or(&subject);
                    if current.as_deref() == Some(imported)
                        && !labels.is_empty()
                        && self.stimulation_tab.is_open()
                        && !self.stimulation_tab.is_dirty()
                        && !self.stage_panel.running
                    {
                        self.start_stimulation(ctx);
                    }
                }
                ReportImportAction::Closed => {}
            }
        }
    }

    fn import_block(&self) -> Option<&'static str> {
        import_block(ImportBlockState {
            running: self.stage_panel.running,
            batch_active: self.batch.is_some(),
            review_open: self.review_prepare.is_some()
                || self.review_panel.session_stage().is_some(),
            review_finishing: self.review_finishing.load(Ordering::SeqCst) > 0,
            refine_open: refine_prepare_blocks(
                self.refine_prepare.is_some(),
                self.refine_prepare_follow,
            ) || self.refine_tab.has_session(),
            refine_saving: self.refine_busy.load(Ordering::SeqCst) > 0
                || self.refine_save.is_some(),
            group_calculating: self.group_tab.is_calculating(),
        })
    }

    fn importing(&self) -> bool {
        self.import.as_ref().is_some_and(ImportWizard::importing)
    }

    fn open_import_wizard(&mut self) {
        if self.import.is_some() {
            return;
        }
        if let Some(reason) = self.import_block() {
            self.push_error("Cannot import images now", reason);
            return;
        }
        let mut wizard = ImportWizard::new(&self.state.bids_dir);
        wizard.output_dirs = self.state.output_dirs.clone();
        self.import = Some(wizard);
    }

    fn apply_import_action(&mut self, action: ImportWizardAction) {
        match action {
            ImportWizardAction::None => {}
            ImportWizardAction::Close => self.import = None,
            ImportWizardAction::Imported {
                root,
                subject_folder,
                invalidated,
                postop_modality,
            } => {
                self.import = None;
                if let Some(m) = postop_modality {
                    self.state.stage.postop_modality = m;
                }
                if let Some(line) = invalidated {
                    self.log_panel.push(line);
                }
                self.refresh();
                if self.stage_panel.running {
                    self.push_error(
                        "Images imported",
                        format!(
                            "{subject_folder} was imported into {root}. A run is in progress, so \
                             the subject was not opened; open it when the run has finished."
                        ),
                    );
                    return;
                }
                self.remember_bids_dir(&root);
                self.open_subject(&root, &subject_folder);
            }
        }
    }

    fn apply_landing_action(&mut self, action: LandingAction, ctx: &egui::Context) {
        let changes_subject = matches!(
            action,
            LandingAction::PickFolder
                | LandingAction::OpenFolder(_)
                | LandingAction::OpenSubject { .. }
        );
        if changes_subject && self.stage_panel.running {
            self.push_error("Cannot change the subject now", SUBJECT_LOCKED);
            return;
        }
        match action {
            LandingAction::None => {}
            LandingAction::PickFolder => self.open_bids_picker(ctx),
            LandingAction::OpenFolder(dir) => {
                self.remember_bids_dir(&dir);
                self.landing.subjects_open = true;
            }
            LandingAction::OpenSubject { bids_dir, subject } => {
                self.open_subject(&bids_dir, &subject);
            }
            LandingAction::ImportImages => self.open_import_wizard(),
            LandingAction::Skip => {
                self.show_landing = false;
                self.guided = false;
            }
        }
    }

    fn apply_guided_nav(&mut self, nav: GuidedNav, ctx: &egui::Context) {
        match nav {
            GuidedNav::None => {}
            GuidedNav::Back => {
                if let Some(prev) = self.guided_step.prev() {
                    self.guided_step = prev;
                }
            }
            GuidedNav::Next => {
                if let Some(next) = self.guided_step.next() {
                    self.guided_step = next;
                }
            }
            GuidedNav::Jump(step) => self.guided_step = step,
            GuidedNav::FullView | GuidedNav::Finish => self.guided = false,
            GuidedNav::ChangeSubject => self.show_landing = true,
            GuidedNav::ImportImages => self.open_import_wizard(),
            GuidedNav::OpenViewer => {
                self.guided_vta_want = self.guided_vta_label();
                if self.readiness.get().has_scene_manifest {
                    self.guided = false;
                    self.focus_tab(TabKind::Scene);
                    if let Err(e) =
                        self.scene_panel
                            .load_default_manifest(&mut self.state, ctx, false)
                    {
                        self.push_error("3D viewer", e);
                    }
                } else if let Some(mut req) = self
                    .stage_panel
                    .build_request(&self.state, &[Stage::Visualize])
                {
                    req.overwrite_approved = false;
                    self.open_viewer_after_visualize = true;
                    if !self.spawn_run(req, ctx) {
                        self.open_viewer_after_visualize = false;
                    }
                }
            }
        }
    }

    /// Ctrl+R in the guide runs the step on screen, not the whole selection.
    fn run_guided_step(&mut self, ctx: &egui::Context) {
        let Some(stage) = self.guided_step.stage() else {
            return;
        };
        let step = self.guided_step;
        if self.stage_panel.running {
            self.push_error("Cannot start the run", RUN_ALREADY_ACTIVE);
            return;
        }
        let readiness = self.readiness.get().clone();
        if !guided_can_run(&self.state.stage, &readiness, stage) {
            let why = guided_refusal(&self.state.stage, &readiness, stage);
            self.push_error("Cannot start this step yet", why);
            return;
        }
        let rerun = guided_step_rerun(&readiness, &self.progress, stage);
        let action = guided_step_action(&self.stage_panel, &self.state, &readiness, step, rerun);
        self.apply_stage_action(action, ctx);
    }

    fn guided_vta_label(&self) -> Option<String> {
        let readiness = self.readiness.get();
        crate::guided::chosen_stimulation(readiness, self.state.guided_stimulation.as_deref())
            .filter(|l| crate::guided::stimulation_computed(readiness, Some(l)))
    }

    fn sync_guided_vta(&mut self, ctx: &egui::Context) {
        if let Some(load) = &self.guided_vta_load {
            match load.rx.try_recv() {
                Ok(surfaces) => {
                    let load = self.guided_vta_load.take();
                    if let Some(load) = load {
                        if self.scene_panel.scene_key() == Some(load.key) {
                            self.scene_panel.set_vta_surfaces(surfaces);
                            self.guided_vta_applied = Some((load.label, load.key));
                            self.guided_vta_want = None;
                            ctx.request_repaint();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.guided_vta_load = None;
                    self.guided_vta_want = None;
                }
            }
            return;
        }
        let (Some(label), Some(key)) = (self.guided_vta_want.clone(), self.scene_panel.scene_key())
        else {
            return;
        };
        if !crate::guided::stimulation_computed(self.readiness.get(), Some(&label)) {
            return;
        }
        if self.guided_vta_applied.as_ref() == Some(&(label.clone(), key)) {
            self.guided_vta_want = None;
            return;
        }
        let (Some(subject), bids_dir, output_dir) = (
            self.state.selected_subject.clone(),
            PathBuf::from(&self.state.bids_dir),
            self.state.output_dir(),
        ) else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let worker_label = label.clone();
        let repaint = ctx.clone();
        thread::spawn(move || {
            let surfaces = dbsloc_pipeline::bids::resolve_subject_in(
                &bids_dir,
                output_dir.as_deref(),
                &subject,
                true,
            )
            .ok()
            .and_then(|s| {
                let space = s.norm.template_space.clone()?;
                Some(crate::guided::load_vta_surfaces(
                    &dbsloc_pipeline::vat::settings::label_dir(&s.subj_dir, &space, &worker_label),
                    &worker_label,
                ))
            })
            .unwrap_or_default();
            let _ = tx.send(surfaces);
            repaint.request_repaint();
        });
        self.guided_vta_load = Some(GuidedVtaLoad { label, key, rx });
    }

    fn apply_stage_action(&mut self, action: StagePanelAction, ctx: &egui::Context) {
        match action {
            StagePanelAction::Run(req) => {
                if self.spawn_run(req, ctx) {
                    self.focus_tab(TabKind::Output);
                }
            }
            StagePanelAction::ConfirmOverwriteThenRun(req, stages) => {
                self.ask_overwrite(req, stages);
            }
            StagePanelAction::Cancel => self.request_stop(),
            StagePanelAction::RunBatch(req) => self.request_batch(req, ctx),
            StagePanelAction::SkipBatchReview => {
                if let Some(b) = self.batch.as_mut() {
                    if let Some(item) = b.queue.reviewing().cloned() {
                        b.queue.skip_review(Some("review skipped"));
                        self.review_panel.end_run();
                        self.log_panel.push(format!(
                            "{}: {} review skipped; the subject stays awaiting review",
                            item.subject,
                            item.stage.label()
                        ));
                    }
                }
            }
            StagePanelAction::EndBatch => {
                if let Some(b) = self.batch.as_mut() {
                    let open = b.queue.reviewing().is_some();
                    b.queue.abandon();
                    if open {
                        self.review_panel.end_run();
                    }
                    self.log_panel
                        .push("Run of several subjects ended by the user.");
                }
            }
            StagePanelAction::DismissBatchResults => self.last_batch = None,
            StagePanelAction::ReviewNow => {
                self.show_landing = false;
                self.guided = false;
                self.focus_tab(TabKind::Review);
            }
            StagePanelAction::ReviewStage(stage) => self.start_review_prepare(stage, ctx),
            StagePanelAction::RefineLocalization => self.start_refine_prepare(ctx),
            StagePanelAction::OpenStimulation => self.start_stimulation(ctx),
            StagePanelAction::RerunStep(stage) => {
                if self.stage_panel.running {
                    self.push_error("Cannot start the run", RUN_ALREADY_ACTIVE);
                } else {
                    let action = guided_run_action(&self.stage_panel, &self.state, stage, true);
                    self.apply_stage_action(action, ctx);
                }
            }
            StagePanelAction::BrowseTemplate => self.open_template_picker(ctx),
            StagePanelAction::BrowseTemplateT2 => self.open_template_t2_picker(ctx),
            StagePanelAction::RevealOutputDirectory => match self.subject_derivatives_dir() {
                Some(dir) => {
                    if let Err(e) = reveal_in_file_manager(&dir) {
                        self.push_error("Could not open the output directory", e);
                    }
                }
                None => self.push_error(
                    "Could not open the output directory",
                    "No subject is selected.",
                ),
            },
            StagePanelAction::OpenStageFolder(stage) => {
                let folder = crate::panels::stage_output_folder(stage)
                    .and_then(|f| self.subject_derivatives_dir().map(|d| d.join(f)));
                match folder {
                    Some(dir) => {
                        if let Err(e) = reveal_in_file_manager(&dir) {
                            self.push_error("Could not open the folder", e);
                        }
                    }
                    None => self.push_error("Could not open the folder", "No subject is selected."),
                }
            }
            StagePanelAction::None => {}
        }
    }

    /// Keep the recent-subjects list current: a subject counts once the dataset
    /// scan has confirmed it and the path field has stopped changing.
    fn remember_selected_subject(&mut self) {
        let Some(subject) = self.state.selected_subject.clone() else {
            return;
        };
        let bids = self.state.bids_dir.trim().to_owned();
        let newest = self.state.recent_subjects.first();
        if newest.is_some_and(|r| r.bids_dir == bids && r.subject == subject) {
            return;
        }
        if self.subject_panel.lists(&bids, &subject) {
            remember_subject_in_state(&mut self.state, &bids, &subject);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};

        let dialog_open = self.confirm_quit
            || self.confirm_overwrite.is_some()
            || !self.errors.is_empty()
            || self.show_shortcuts;
        if dialog_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.confirm_quit {
                self.confirm_quit = false;
            } else if self.show_shortcuts {
                self.show_shortcuts = false;
            } else if self.confirm_overwrite.is_some() {
                self.confirm_overwrite = None;
            } else {
                self.errors.clear();
            }
            return;
        }

        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F1))) {
            self.show_shortcuts = !self.show_shortcuts;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::NONE, Key::F5))) {
            self.refresh();
        }
        let import_closed = self.import.is_none();
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::R)))
            && self.confirm_overwrite.is_none()
            && import_closed
        {
            self.run_now(ctx);
        }
        if (self.stage_panel.running || self.refine_tab.is_saving())
            && ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::Period)))
        {
            if self.stage_panel.running {
                self.request_stop();
            } else {
                self.refine_cancel.store(true, Ordering::SeqCst);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::O))) && import_closed {
            if self.stage_panel.running {
                self.log_panel.push(SUBJECT_LOCKED);
            } else {
                self.open_bids_picker(ctx);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::H)))
            && self.import.is_none()
        {
            self.show_landing = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::I))) {
            self.open_import_wizard();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::L))) {
            self.log_panel.clear();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL | Modifiers::SHIFT, Key::C))) {
            self.log_panel.copy_all(ctx);
        }
        for (idx, key) in [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
        ]
        .iter()
        .enumerate()
        {
            if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, *key))) && import_closed {
                self.show_landing = false;
                self.guided = false;
                self.focus_tab(TabKind::ALL[idx]);
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::CTRL, Key::Num7))) && import_closed {
            self.open_group_analysis();
        }
        for (key, delta) in [(Key::ArrowDown, 1), (Key::ArrowUp, -1)] {
            if ctx.input_mut(|i| i.consume_shortcut(&sc(Modifiers::ALT, key))) && import_closed {
                if self.stage_panel.running {
                    self.log_panel.push(SUBJECT_LOCKED);
                } else {
                    self.subject_panel.step_selection(&mut self.state, delta);
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let unit = em(ui);
        let pal = palette(ui);
        ui.add_space(unit * 0.2);

        ui.horizontal(|ui| {
            show_logo(ui, &self.logo);
            ui.add(
                egui::Label::new(egui::RichText::new("Wyss DBS Localizer").heading().strong())
                    .selectable(false),
            );
            if ui
                .add_enabled(
                    !self.show_landing && self.import.is_none(),
                    egui::Button::new("Home"),
                )
                .on_hover_text("Back to the start screen (Ctrl+H)")
                .clicked()
            {
                self.show_landing = true;
            }
            let wizard_closed = self.import.is_none();
            let has_subject = self.state.selected_subject.is_some()
                && !self.state.bids_dir.trim().is_empty();
            let import_block = self.import_block();
            ui.add_enabled_ui(wizard_closed, |ui| {
                ui.menu_button("Import…", |ui| {
                    if ui
                        .add_enabled(
                            import_block.is_none(),
                            egui::Button::new("Images into a subject…"),
                        )
                        .on_hover_text("Create a subject from NIfTI files or DICOM folders (Ctrl+I)")
                        .on_disabled_hover_text(import_block.unwrap_or_default())
                        .clicked()
                    {
                        self.open_import_wizard();
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_subject, egui::Button::new("Session report (PDF)…"))
                        .on_hover_text(
                            "Read the stimulation settings of a vendor session report into the open subject",
                        )
                        .on_disabled_hover_text("Open a subject first")
                        .clicked()
                    {
                        self.open_report_picker(ctx);
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Import images or a session report (Ctrl+I opens the image wizard)");
            });
            if ui
                .button("Connectomes…")
                .on_hover_text("Download, import and list normative connectomes")
                .clicked()
            {
                self.connectome_panel.open();
            }
            if ui
                .button("Group analysis")
                .on_hover_text(
                    "Patients, group stimulation, volumes of tissue activated and the group 3D view (Ctrl+7)",
                )
                .clicked()
            {
                self.open_group_analysis();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Help")
                    .on_hover_text("Keyboard shortcuts and system information (F1)")
                    .clicked()
                {
                    self.show_shortcuts = true;
                }
                ui.menu_button("Settings", |ui| {
                    ui.label(egui::RichText::new("Theme").small().color(pal.text_muted));
                    for t in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
                        ui.selectable_value(&mut self.state.theme, t, t.label())
                            .on_hover_text("Colour theme");
                    }
                    ui.separator();
                    if ui
                        .button("Reset panel layout")
                        .on_hover_text("Restore the default arrangement of the panels")
                        .clicked()
                    {
                        self.dock = default_dock_state();
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Colour theme and panel layout");
            });
        });

        let dataset_locked = self.stage_panel.running.then_some(SUBJECT_LOCKED);
        let input_row = DirectoryRow {
            label: "Input directory",
            field_id: egui::Id::new("input_directory_field"),
            hint: "Path to the BIDS dataset".to_owned(),
            hover: "BIDS dataset: the directory that holds rawdata/ (Ctrl+O)",
            browse_hover: "Choose the input directory (Ctrl+O)",
            recent_hover: "Recently used input directories",
            recent_id: "recent_bids_dirs",
            recents: &self.state.recent_bids_dirs.clone(),
            empty_recents: "No recent input directories",
            default_entry: None,
            disabled: dataset_locked,
        };
        match directory_row(ui, &input_row, &mut self.state.bids_dir) {
            DirectoryEvent::Committed => {
                let bids = self.state.bids_dir.clone();
                self.remember_bids_dir(&bids);
            }
            DirectoryEvent::Browse => self.open_bids_picker(ctx),
            DirectoryEvent::Pick(dir) => self.remember_bids_dir(&dir),
            DirectoryEvent::PickDefault | DirectoryEvent::None => {}
        }

        let output_field_id = egui::Id::new("output_directory_field");
        if !ui.memory(|m| m.has_focus(output_field_id)) {
            self.state.output_dir_edit = self
                .state
                .output_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
        }
        let bids = self.state.bids_dir.trim().to_owned();
        let output_lock = if bids.is_empty() {
            Some("Choose the input directory first.")
        } else {
            self.output_folder_lock()
        };
        let default_hint = if bids.is_empty() {
            "derivatives/leaddbs inside the input directory".to_owned()
        } else {
            dbsloc_pipeline::paths::derivatives_root_lexical(Path::new(&bids), None)
                .display()
                .to_string()
        };
        let output_row = DirectoryRow {
            label: "Output directory",
            field_id: output_field_id,
            hint: default_hint,
            hover: OUTPUT_DIRECTORY_HOVER,
            browse_hover: "Choose the directory that receives the results of this dataset",
            recent_hover: "Recently used output directories",
            recent_id: "recent_output_dirs",
            recents: &self.state.recent_output_dirs.clone(),
            empty_recents: "No recent output directories",
            default_entry: Some("Default (inside the input directory)"),
            disabled: output_lock,
        };
        let mut edit = std::mem::take(&mut self.state.output_dir_edit);
        let event = directory_row(ui, &output_row, &mut edit);
        self.state.output_dir_edit = edit;
        match event {
            DirectoryEvent::Committed => {
                let text = self.state.output_dir_edit.trim().to_owned();
                if text.is_empty() {
                    self.change_output_dir(None);
                } else {
                    self.change_output_dir(Some(&text));
                }
            }
            DirectoryEvent::Browse => self.open_output_picker(ctx),
            DirectoryEvent::Pick(dir) => self.change_output_dir(Some(&dir)),
            DirectoryEvent::PickDefault => self.change_output_dir(None),
            DirectoryEvent::None => {}
        }
        ui.add_space(unit * 0.2);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let unit = em(ui);
        let pal = palette(ui);
        ui.add_space(unit * 0.1);
        ui.horizontal(|ui| {
            let dataset = Path::new(&self.state.bids_dir)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "no dataset".to_owned());
            let subject = self
                .state
                .selected_subject
                .clone()
                .unwrap_or_else(|| "no subject".to_owned());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{dataset} · {subject}"))
                        .small()
                        .color(pal.text_muted),
                )
                .truncate()
                .selectable(false),
            )
            .on_hover_text(if self.state.bids_dir.is_empty() {
                "No input directory chosen".to_owned()
            } else {
                self.state.bids_dir.clone()
            });
            ui.separator();
            let electrode = if self.state.stage.el_model_from_subject {
                "electrode model from each subject".to_owned()
            } else {
                self.state
                    .stage
                    .el_model
                    .clone()
                    .unwrap_or_else(|| "no electrode model".to_owned())
            };
            ui.add(
                egui::Label::new(egui::RichText::new(electrode).small().color(pal.text_muted))
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text("Electrode model used by the Reconstruct step");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("v{}", dbsloc_pipeline::version::VERSION))
                            .small()
                            .color(pal.text_muted),
                    )
                    .selectable(false),
                )
                .on_hover_text(version_line());
                ui.separator();
                let batch_label = self.batch.as_ref().and_then(|b| b.queue.progress_label());
                let (glyph, text, color) =
                    if let (true, Some(label)) = (self.stage_panel.running, batch_label) {
                        (
                            "▶",
                            format!("{label} · {}", self.progress.progress_text()),
                            pal.info,
                        )
                    } else if self.stage_panel.running {
                        ("▶", self.progress.progress_text(), pal.info)
                    } else if let Some(summary) = self.progress.summary() {
                        if self.progress.any_failed() {
                            ("✖", format!("Last run: {summary}"), pal.err)
                        } else if self.progress.any_rejected() {
                            ("⚠", format!("Last run: {summary}"), pal.warn)
                        } else {
                            ("✔", format!("Last run: {summary}"), pal.ok)
                        }
                    } else {
                        ("○", "Idle".to_owned(), pal.text_muted)
                    };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{glyph} {text}"))
                            .small()
                            .color(color),
                    )
                    .selectable(false),
                )
                .on_hover_text("State of the most recent pipeline run");
            });
        });
        ui.add_space(unit * 0.1);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if !self.errors.is_empty() {
            let mut dismiss_all = false;
            let mut dismiss_one: Option<usize> = None;
            let err_color = tokens::palette(ctx.global_style().visuals.dark_mode).err;
            let mut open = true;
            let grouped = group_errors(&self.errors);
            egui::Window::new(format!("⚠ Problems ({})", grouped.len()))
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(ctx.global_style().spacing.interact_size.y * 30.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(ui.text_style_height(&egui::TextStyle::Body) * 18.0)
                        .show(ui, |ui| {
                            for g in &grouped {
                                ui.label(egui::RichText::new(&g.title).strong().color(err_color));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&g.body).monospace().small(),
                                    )
                                    .wrap(),
                                );
                                ui.horizontal(|ui| {
                                    if ui
                                        .button("Copy")
                                        .on_hover_text("Copy this message to the clipboard")
                                        .clicked()
                                    {
                                        ctx.copy_text(format!("{}\n{}", g.title, g.body));
                                    }
                                    if ui
                                        .button("Dismiss")
                                        .on_hover_text("Remove this message")
                                        .clicked()
                                    {
                                        dismiss_one = Some(g.first_index);
                                    }
                                });
                                ui.separator();
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .button("Copy all")
                            .on_hover_text("Copy every message to the clipboard")
                            .clicked()
                        {
                            let all = grouped
                                .iter()
                                .map(|g| format!("{}\n{}", g.title, g.body))
                                .collect::<Vec<_>>()
                                .join("\n\n");
                            ctx.copy_text(all);
                        }
                        if ui.button("Dismiss all").clicked() {
                            dismiss_all = true;
                        }
                    });
                });
            if dismiss_all || !open {
                self.errors.clear();
            } else if let Some(i) = dismiss_one {
                if let Some(body) = self.errors.get(i).map(|e| e.body.clone()) {
                    self.errors.retain(|e| e.body != body);
                }
            }
        }

        if self.confirm_overwrite.as_ref().is_some_and(|p| {
            !p.belongs_to(&self.state.bids_dir, self.state.selected_subject.as_deref())
        }) {
            self.confirm_overwrite = None;
            self.log_panel
                .push("The subject changed, so the question to replace its results was closed.");
        }
        if let Some(pending) = &self.confirm_overwrite {
            let names: Vec<&str> = pending.stages.iter().map(|s| s.label()).collect();
            let mut confirmed = false;
            let mut cancelled = false;
            let batch_count = pending.batch.as_ref().map(Vec::len);
            egui::Window::new("Replace existing results?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(match batch_count {
                        Some(n) => format!(
                            "\"Rerun all selected steps\" is ticked: for each of the {n} subjects, \
                             existing output of these steps will be overwritten:"
                        ),
                        None => {
                            "These steps already have output that will be overwritten:".to_owned()
                        }
                    });
                    ui.add_space(4.0);
                    for n in &names {
                        ui.label(egui::RichText::new(format!("• {n}")).monospace());
                    }
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("This cannot be undone from inside the application.")
                            .small(),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new("Replace and run").strong())
                            .on_hover_text("Run the selected steps and overwrite their output")
                            .clicked()
                        {
                            confirmed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                    });
                });
            if confirmed {
                if let Some(pending) = self.confirm_overwrite.take() {
                    let started = match pending.batch {
                        Some(subjects) => {
                            self.start_batch(pending.req, subjects, ctx);
                            self.batch.is_some()
                        }
                        None => self.spawn_run(pending.req, ctx),
                    };
                    if started {
                        self.focus_tab(TabKind::Output);
                    }
                }
            } else if cancelled {
                self.confirm_overwrite = None;
            }
        }

        if self.confirm_batch_stop {
            let mut choice: Option<bool> = None;
            let mut keep = false;
            let (running, queued) = self.batch.as_ref().map_or((String::new(), 0), |b| {
                (
                    b.queue
                        .current()
                        .map(|r| r.subject.clone())
                        .unwrap_or_default(),
                    b.queue.pending_len(),
                )
            });
            egui::Window::new("Stop the run of several subjects?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{running} is running; {queued} more subject{} queued.",
                        if queued == 1 { " is" } else { "s are" }
                    ));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new("Stop after this subject").strong())
                            .on_hover_text(
                                "Let the running subject finish all its steps, then start no further subject",
                            )
                            .clicked()
                        {
                            choice = Some(false);
                        }
                        if ui
                            .button("Stop now")
                            .on_hover_text(
                                "End the running step at once, skip the rest of this subject and every queued subject",
                            )
                            .clicked()
                        {
                            choice = Some(true);
                        }
                        if ui
                            .button("Keep running")
                            .on_hover_text("Close this question and let the batch go on")
                            .clicked()
                        {
                            keep = true;
                        }
                    });
                });
            if let Some(now) = choice {
                self.confirm_batch_stop = false;
                if let Some(b) = self.batch.as_mut() {
                    b.queue.stop_after_current();
                }
                if now {
                    self.pipeline_cancel.store(true, Ordering::SeqCst);
                    self.stage_panel.stopping = true;
                    self.log_panel.push(
                        "Stop requested — ending the current step; no further subject starts.",
                    );
                } else {
                    self.log_panel
                        .push("The batch stops once the running subject has finished.");
                }
            } else if keep || !self.stage_panel.running {
                self.confirm_batch_stop = false;
            }
        }

        if self.show_shortcuts {
            let mut open = true;
            let unit = egui::TextStyle::Body.resolve(&ctx.global_style()).size;
            let max_height = ctx.content_rect().height() * 0.85;
            egui::Window::new("Help")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(unit * 38.0)
                .max_height(max_height)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                  egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    for (group, entries) in SHORTCUTS {
                        let mut rows: Vec<(&str, &str)> =
                            entries.iter().map(|s| (s.keys, s.action)).collect();
                        if *group == "3D Viewer" {
                            rows.extend(
                                self.state
                                    .navigation
                                    .help_rows()
                                    .iter()
                                    .map(|(what, how)| (*how, *what)),
                            );
                        }
                        shortcut_group(ui, group, &rows);
                    }
                    let mut review_rows: Vec<(&str, &str)> = vec![(
                        "F11",
                        "Open the registration review in a separate window; there, F11 toggles full screen and Esc leaves it",
                    )];
                    review_rows.extend_from_slice(dbsloc_viz::checkreg_app::shortcut_rows());
                    shortcut_group(ui, "Registration review", &review_rows);
                    let mut refine_rows: Vec<(&str, &str)> = vec![(
                        "F11",
                        "Open Refine localization in a separate window; there, F11 toggles full screen and Esc leaves it",
                    )];
                    refine_rows.extend_from_slice(dbsloc_viz::refine_app::shortcut_rows());
                    shortcut_group(ui, "Refine localization", &refine_rows);
                    shortcut_group(
                        ui,
                        "Image preview",
                        dbsloc_viz::volume_preview::shortcut_rows(),
                    );
                    ui.separator();
                    let mut rows = vec![("Version".to_owned(), version_line())];
                    rows.extend(self.stage_panel.gpu_help.system_rows());
                    system_group(ui, &rows);
                    crate::app_log::help_rows(ui);
                  });
                });
            self.show_shortcuts = open;
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct TabBusy {
    review: bool,
    refine: bool,
    stimulation: bool,
    group: bool,
}

fn tab_closeable(tab: TabKind, busy: TabBusy) -> bool {
    match tab {
        TabKind::Subjects | TabKind::Pipeline | TabKind::Output | TabKind::Scene => false,
        TabKind::Review => !busy.review,
        TabKind::Refine => !busy.refine,
        TabKind::Stimulation => !busy.stimulation,
        TabKind::Group => !busy.group,
    }
}

struct AppTabs<'a> {
    refine_preparing: bool,
    state: &'a mut DbslocAppState,
    subject_panel: &'a mut SubjectPanel,
    stage_panel: &'a mut StagePanel,
    log_panel: &'a mut LogPanel,
    scene_panel: &'a mut ScenePanel,
    review_panel: &'a mut ReviewPanel,
    refine_tab: &'a mut RefineTab,
    stimulation_tab: &'a mut crate::stimulation_panel::StimulationTab,
    stim_env: crate::stimulation_panel::StimEnv,
    stimulation_output: crate::stimulation_panel::StimOutput,
    group_tab: &'a mut crate::group_panel::GroupTab,
    group_env: crate::group_panel::GroupEnv,
    group_output: crate::group_panel::GroupOutput,
    readiness: &'a PipelineReadiness,
    progress: &'a RunProgress,
    frame: &'a mut eframe::Frame,
    ctx: egui::Context,
    stage_action: StagePanelAction,
    scene_action: ScenePanelAction,
    review_output: ReviewOutput,
    refine_output: RefineTabOutput,
}

impl TabViewer for AppTabs<'_> {
    type Tab = TabKind;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(tab.title())
    }

    fn on_tab_button(&mut self, tab: &mut Self::Tab, response: &egui::Response) {
        response.clone().on_hover_text(tab.hint());
    }

    /// Panels scroll their own content; an outer scroll area would break the
    /// 3D viewer's available-height calculation and add a second scrollbar.
    fn scroll_bars(&self, _tab: &Self::Tab) -> [bool; 2] {
        [false, false]
    }

    /// Subjects, Pipeline, Output and 3D Viewer are fixed parts of the application. The
    /// other four come and go and can be closed while they hold nothing.
    fn is_closeable(&self, tab: &Self::Tab) -> bool {
        tab_closeable(
            *tab,
            TabBusy {
                review: self.review_panel.session_stage().is_some(),
                refine: self.refine_tab.has_session()
                    || self.refine_tab.is_saving()
                    || self.refine_preparing,
                stimulation: self.stimulation_tab.is_open() || self.stimulation_tab.is_preparing(),
                group: self.group_tab.is_busy(),
            },
        )
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            TabKind::Subjects => {
                let locked = self.stage_panel.running.then_some(SUBJECT_LOCKED);
                self.subject_panel.show(ui, self.state, locked);
            }
            TabKind::Pipeline => {
                let action = self
                    .stage_panel
                    .show(ui, self.state, self.readiness, self.progress);
                if !matches!(action, StagePanelAction::None) {
                    self.stage_action = action;
                }
            }
            TabKind::Output => {
                let mut follow = self.state.follow_output;
                let mut detail = self.state.output_detail;
                let mut wrap = self.state.wrap_output;
                self.log_panel
                    .show(ui, &self.ctx, &mut detail, &mut follow, &mut wrap);
                self.state.follow_output = follow;
                self.state.output_detail = detail;
                self.state.wrap_output = wrap;
            }
            TabKind::Scene => {
                let action = self.scene_panel.show(ui, self.frame, &self.ctx, self.state);
                if action != ScenePanelAction::None {
                    self.scene_action = action;
                }
            }
            TabKind::Review => {
                let out = self.review_panel.show(ui, &self.ctx);
                self.review_output.merge(out);
            }
            TabKind::Refine => {
                let out = self.refine_tab.show(ui, &self.ctx);
                self.refine_output.merge(out);
            }
            TabKind::Stimulation => {
                let out = self.stimulation_tab.show(ui, &self.ctx, self.stim_env);
                self.stimulation_output.merge(out);
            }
            TabKind::Group => {
                let out = self.group_tab.show(ui, &self.ctx, &self.group_env);
                self.group_output.merge(out);
            }
        }
    }
}

impl eframe::App for DbslocApp {
    fn persist_egui_memory(&self) -> bool {
        !self.screenshot_mode
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.persist_state_if_changed(true);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.screenshot_focus_hook(&ctx);

        let focused = ctx.input(|i| i.focused);
        if focused && !self.window_was_focused && !self.screenshot_mode {
            self.refresh();
        }
        self.window_was_focused = focused;

        if self.applied_theme != Some(self.state.theme) {
            ctx.set_theme(self.state.theme.preference());
            self.applied_theme = Some(self.state.theme);
        }

        self.poll_file_picks();
        if let Some(rx) = &self.pending_report_pick {
            if let Ok(paths) = rx.try_recv() {
                self.pending_report_pick = None;
                self.open_report_dialog(paths, &ctx);
            }
        }
        let import_end = self.import.as_mut().and_then(|w| {
            w.poll(&ctx);
            w.take_import_end()
        });
        if let Some(end) = import_end {
            self.import_ended(end);
        }
        {
            let st = &mut self.state;
            st.template_ui.poll(
                &mut st.normalization_template,
                &mut st.normalization_template_t2,
            );
            if let Some(dir) = st.atlas_dir_pick.poll() {
                st.stage.leaddbs_atlases_dir = Some(dir);
            }
            self.connectomes_setting_push.push(
                st.stage.leaddbs_connectomes_dir.as_deref(),
                crate::connectome_roots::set_setting,
            );
        }
        self.scene_panel.poll_load(&ctx);
        let scan_key = (
            self.state.bids_dir.clone(),
            self.state.selected_subject.clone(),
            self.state.normalization_template.clone(),
        );
        if self.scan_key.as_ref() != Some(&scan_key) {
            self.scan_key = Some(scan_key);
            self.scan_settled_at = Instant::now();
        }
        if !self.gpu_probe_seen {
            if dbsloc_pipeline::reg_backend::cached_probe_if_ready().is_some() {
                self.gpu_probe_seen = true;
                self.readiness.invalidate();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        self.readiness.forget_other_subject(&self.state);
        let settled = self.scan_settled_at.elapsed() >= SCAN_DEBOUNCE;
        if settled {
            self.subject_panel.poll(&ctx, &self.state);
            self.readiness.poll(&ctx, &self.state);
        } else {
            ctx.request_repaint_after(SCAN_DEBOUNCE);
        }
        self.stage_panel.run_wait = self.run_wait();
        self.handle_shortcuts(&ctx);
        if settled {
            self.remember_selected_subject();
        }

        self.log_gpu_status_once();
        if let Some(rx) = &self.batch_plan_rx {
            if let Ok(stages) = rx.try_recv() {
                self.progress
                    .start_with(&stages, self.log_panel.defer_review);
                if let Some(b) = self.batch.as_mut() {
                    b.queue.set_planned(&stages);
                }
                self.batch_plan_rx = None;
            }
        }
        let events = self.log_panel.poll_events(&mut self.stage_panel);
        let mut reload_scene = false;
        let batch_subject = self
            .batch
            .as_ref()
            .and_then(|b| b.queue.current().map(|r| r.subject.clone()));
        for ev in &events {
            if let Some(b) = self.batch.as_mut() {
                b.tally.on_event(ev);
            }
            let paired_with_rejection = matches!(
                ev,
                RunEvent::StageFailed { stage, .. } if self.progress.is_rejected_label(stage)
            );
            self.progress.on_event(ev);
            match ev {
                RunEvent::StageCompleted { stage, .. } => {
                    self.readiness.invalidate();
                    let owner = self
                        .run_owner
                        .as_ref()
                        .map(|o| (o.bids_dir.as_path(), o.subject.as_str()));
                    self.stimulation_tab.stage_completed(stage, owner, &ctx);
                    if stage == Stage::Vat.label()
                        && self.guided
                        && self.guided_step == GuidedStep::Stimulation
                    {
                        self.guided_vta_want = crate::guided::chosen_stimulation(
                            self.readiness.get(),
                            self.state.guided_stimulation.as_deref(),
                        );
                    }
                    if stage == Stage::Reconstruct.label() && self.open_viewer_after_reconstruct {
                        self.open_viewer_after_reconstruct = false;
                        reload_scene = true;
                    }
                    if stage == Stage::Visualize.label() && self.open_viewer_after_visualize {
                        self.open_viewer_after_visualize = false;
                        self.guided = false;
                        reload_scene = true;
                    }
                }
                RunEvent::StageFailed { stage, message } if !paired_with_rejection => {
                    self.errors.push(ErrorEntry {
                        title: match &batch_subject {
                            Some(sub) => format!("{sub}: step failed: {stage}"),
                            None => format!("Step failed: {stage}"),
                        },
                        body: crate::panels::humanize_message(message),
                    });
                }
                RunEvent::PipelineCompleted { .. } => {
                    self.readiness.invalidate();
                    self.group_tab.rescan();
                    self.stimulation_tab.run_ended();
                    self.review_panel.end_run();
                    self.review_rx = None;
                    self.open_viewer_after_reconstruct = false;
                    self.open_viewer_after_visualize = false;
                    self.finish_batch_subject(false);
                }
                _ => {}
            }
        }
        if self.log_panel.take_unexpected_end() {
            self.finish_batch_subject(true);
            self.progress.abandon();
            self.readiness.invalidate();
            self.stimulation_tab.run_ended();
            self.review_panel.end_run();
            self.review_rx = None;
            self.open_viewer_after_reconstruct = false;
            self.open_viewer_after_visualize = false;
            crate::app_log::error("the pipeline stopped without reporting the end of the run");
            self.push_error(
                "The pipeline stopped unexpectedly",
                "The run ended without reporting a result for the step that was running. \
                 Its output may be incomplete. The Output tab and the session log (F1) show \
                 the last messages.",
            );
        }
        for (title, body) in crate::app_log::take_ui_errors() {
            self.push_error(title, body);
        }
        self.quit_once_the_run_has_stopped(&ctx);
        self.poll_review_requests();
        self.poll_review_prepare();
        self.note_jobs();
        self.poll_refine_prepare();
        self.poll_refine_save(&ctx);
        self.follow_selection(&ctx);
        self.stage_panel.run_wait = self.run_wait();
        self.stage_panel.refine_activity = if self.refine_prepare.is_some() {
            RefineActivity::Preparing
        } else if self.refine_save.is_some() {
            RefineActivity::Saving
        } else {
            RefineActivity::Idle
        };
        if reload_scene {
            self.focus_tab(TabKind::Scene);
            if let Err(e) = self
                .scene_panel
                .load_default_manifest(&mut self.state, &ctx, true)
            {
                self.push_error("3D viewer", e);
            }
        }
        if self.stage_panel.running {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.stage_panel.batch = self.batch_info();

        if std::mem::take(&mut self.state.leaddbs_import_request) {
            self.leaddbs_import.open(false);
        }
        let import_events = self.leaddbs_import.show(&ctx, &mut self.state);
        if import_events.refresh {
            self.connectome_panel.refresh();
        }
        if import_events.open_connectomes {
            self.connectome_panel.open();
        }
        self.connectome_panel.show(&ctx, &mut self.state);
        if let Some(dialog) = self.screenshot_consent.as_mut() {
            let _ = dialog.show(&ctx);
        }
        self.stimulation_tab.set_connectome_context(
            self.state.stage.connectomes_dir.as_deref(),
            self.state.stim_connectome.as_deref(),
            self.state.connectome_cache.setting(),
        );
        self.group_tab
            .set_cache(self.state.connectome_cache.setting());
        if self.scene_panel.take_fibre_file_request() {
            self.fibre_files.request(&ctx);
        }
        for result in self.fibre_files.poll() {
            match result {
                Ok(layer) => {
                    self.scene_panel.upsert_fibre_layer(layer);
                }
                Err(e) => self.push_error("Fibre file", e),
            }
        }
        self.image_preview.show(&ctx);
        crate::atlas_download::show_window(&ctx);
        if crate::report_import::take_import_request(&ctx) {
            self.open_report_picker(&ctx);
        }
        let report_actions = self.report_import.show(&ctx);
        self.apply_report_actions(report_actions, &ctx);
        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui, &ctx));
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        let canvas = tokens::palette(ctx.global_style().visuals.dark_mode).canvas;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(canvas))
            .show(ui, |ui| {
                let import_block = self.import_block();
                if let Some(w) = self.import.as_mut() {
                    w.run_block = import_block;
                    let action = w.show(ui, &ctx);
                    self.apply_import_action(action);
                    return;
                }
                if self.show_landing {
                    self.landing.import_block = import_block;
                    let action = crate::landing::show(
                        ui,
                        &mut self.state,
                        &self.subject_panel,
                        &mut self.landing,
                        &self.logo,
                        &self.stage_panel.gpu_help,
                    );
                    self.apply_landing_action(action, &ctx);
                    return;
                }
                if self.guided {
                    let out = crate::guided::show(
                        ui,
                        GuidedInputs {
                            step: self.guided_step,
                            state: &mut self.state,
                            panel: &self.stage_panel,
                            readiness: self.readiness.get(),
                            progress: &self.progress,
                            review: &mut self.review_panel,
                            import_block,
                        },
                    );
                    self.apply_review_output(out.review, &ctx);
                    self.apply_stage_action(out.action, &ctx);
                    self.apply_guided_nav(out.nav, &ctx);
                    return;
                }
                let stim_env = self.stim_env();
                let group_env = self.group_env();
                let mut tabs = AppTabs {
                    state: &mut self.state,
                    subject_panel: &mut self.subject_panel,
                    stage_panel: &mut self.stage_panel,
                    log_panel: &mut self.log_panel,
                    scene_panel: &mut self.scene_panel,
                    review_panel: &mut self.review_panel,
                    refine_tab: &mut self.refine_tab,
                    stimulation_tab: &mut self.stimulation_tab,
                    stim_env,
                    stimulation_output: crate::stimulation_panel::StimOutput::default(),
                    refine_preparing: self.refine_prepare.is_some(),
                    group_tab: &mut self.group_tab,
                    group_env,
                    group_output: crate::group_panel::GroupOutput::default(),
                    readiness: self.readiness.get(),
                    progress: &self.progress,
                    frame,
                    ctx: ctx.clone(),
                    stage_action: StagePanelAction::None,
                    scene_action: ScenePanelAction::None,
                    review_output: ReviewOutput::default(),
                    refine_output: RefineTabOutput::default(),
                };
                DockArea::new(&mut self.dock)
                    .style(egui_dock::Style::from_egui(ui.style().as_ref()))
                    .show_add_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut tabs);

                let stage_action =
                    std::mem::replace(&mut tabs.stage_action, StagePanelAction::None);
                let scene_action =
                    std::mem::replace(&mut tabs.scene_action, ScenePanelAction::None);
                let review_output = std::mem::take(&mut tabs.review_output);
                let refine_output = std::mem::take(&mut tabs.refine_output);
                let stimulation_output = std::mem::take(&mut tabs.stimulation_output);
                let group_output = std::mem::take(&mut tabs.group_output);
                drop(tabs);
                self.apply_stimulation_output(stimulation_output, &ctx);
                self.apply_group_output(group_output, &ctx);

                self.apply_review_output(review_output, &ctx);
                self.apply_refine_output(refine_output, &ctx);
                self.apply_stage_action(stage_action, &ctx);
                match scene_action {
                    ScenePanelAction::BrowseManifest => self.open_manifest_picker(&ctx),
                    ScenePanelAction::BrowseBackdrop => self.open_backdrop_picker(&ctx),
                    ScenePanelAction::None => {}
                }
            });

        self.note_jobs();
        self.scene_panel.show_popout(&ctx, frame);
        let popout_output = self.review_panel.show_popout(&ctx);
        self.apply_review_output(popout_output, &ctx);
        let refine_popout = self.refine_tab.show_popout(&ctx);
        self.apply_refine_output(refine_popout, &ctx);
        let stim_env = self.stim_env();
        let stim_popout = self.stimulation_tab.show_popout(&ctx, stim_env);
        self.apply_stimulation_output(stim_popout, &ctx);
        let group_env = self.group_env();
        let group_popout = self.group_tab.show_popout(&ctx, &group_env);
        self.apply_group_output(group_popout, &ctx);
        self.sync_group_look();
        self.sync_group_layers(&ctx);
        self.sync_stimulation_scene(&ctx);
        self.sync_guided_vta(&ctx);

        self.dialogs(&ctx);
        self.guard_close_during_run(&ctx);
        if !self.quit_after_run && !self.quit_approved {
            self.tick_batch(&ctx);
        }
        self.persist_state_if_changed(false);
        self.screenshot_hook(&ctx);
    }
}

fn is_nifti_file(path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    (name.ends_with(".nii") || name.ends_with(".nii.gz")) && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_state_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_state(base: &Path, folder: &str, text: &str) {
        let dir = base.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), text).unwrap();
    }

    fn postop_options(
        choice: dbsloc_ui::PostopModality,
        readiness: &dbsloc_ui::PipelineReadiness,
    ) -> dbsloc_pipeline::bids::PostopModality {
        let settings = StageSettings {
            postop_modality: choice,
            ..StageSettings::default()
        };
        let req = settings.run_request(&[Stage::Coregister], None).unwrap();
        pipeline_run_options(
            req,
            PathBuf::from("/data"),
            None,
            "01".to_owned(),
            None,
            false,
            has_both_postop(readiness),
        )
        .postop_modality
    }

    #[test]
    fn postop_choice_only_reaches_a_subject_with_both_postop_modalities() {
        use dbsloc_pipeline::bids::PostopModality as Pipe;
        use dbsloc_ui::{PipelineReadiness, PostopModality as Choice};
        let ct_only = PipelineReadiness {
            has_raw_postop_ct: true,
            ..PipelineReadiness::default()
        };
        let mr_only = PipelineReadiness {
            has_raw_postop_mr: true,
            ..PipelineReadiness::default()
        };
        let both = PipelineReadiness {
            has_raw_postop_ct: true,
            has_raw_postop_mr: true,
            ..PipelineReadiness::default()
        };
        assert_eq!(postop_options(Choice::Mri, &ct_only), Pipe::Auto);
        assert_eq!(postop_options(Choice::Ct, &mr_only), Pipe::Auto);
        assert_eq!(postop_options(Choice::Mri, &both), Pipe::Mri);
        assert_eq!(postop_options(Choice::Ct, &both), Pipe::Ct);
        assert_eq!(postop_options(Choice::Auto, &both), Pipe::Auto);
    }

    #[test]
    fn skip_reason_text_strips_prefixes() {
        assert_eq!(
            skip_reason_text("orientation", "skip:orientation: no directional leads"),
            "no directional leads"
        );
        assert_eq!(
            skip_reason_text(
                "Orientation",
                "skip:orientation: post-op MRI not supported (specs/06)"
            ),
            "post-op MRI not supported (specs/06)"
        );
        assert_eq!(
            skip_reason_text("normalize", "skip:approved"),
            "its output was approved earlier"
        );
        assert_eq!(
            skip_reason_text("vat", "not implemented"),
            "not implemented"
        );
    }

    #[test]
    fn skip_reason_text_survives_multibyte_text() {
        assert_eq!(
            skip_reason_text("orientation", "skip:orientatioé: x"),
            "orientatioé: x"
        );
        assert_eq!(skip_reason_text("orientation", "é"), "é");
        assert_eq!(skip_reason_text("vat", "vaé: y"), "vaé: y");
    }

    #[test]
    fn review_actions_parse() {
        assert_eq!(ScreenshotAction::parse("review"), ScreenshotAction::Review);
        assert_eq!(
            ScreenshotAction::parse("review-guided"),
            ScreenshotAction::ReviewGuided
        );
        assert_eq!(
            ScreenshotAction::parse("review-during-run"),
            ScreenshotAction::ReviewDuringRun
        );
        assert_eq!(
            ScreenshotAction::Review.review_shot(),
            Some(ReviewShot::Docked)
        );
        assert_eq!(ScreenshotAction::Run.review_shot(), None);
    }

    #[test]
    fn screenshot_mode_follows_either_variable() {
        use std::ffi::OsStr;
        assert!(!screenshot_mode_from(None, None));
        assert!(!screenshot_mode_from(Some(OsStr::new("")), Some("  ")));
        assert!(screenshot_mode_from(Some(OsStr::new("a.png")), None));
        assert!(screenshot_mode_from(None, Some("review")));
    }

    #[test]
    fn popout_action_parses() {
        assert_eq!(
            ScreenshotAction::parse("viewer-popout"),
            ScreenshotAction::ViewerPopout
        );
    }

    #[test]
    fn progress_keeps_skip_reason_per_stage() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Normalize]);
        let label = Stage::Normalize.label().to_owned();
        assert_eq!(p.skip_reason(Stage::Normalize), None);
        p.on_event(&RunEvent::StageSkipped {
            stage: label,
            reason: "x".to_owned(),
            result: Some("skip:approved".to_owned()),
        });
        assert_eq!(
            p.skip_reason(Stage::Normalize),
            Some("its output was approved earlier")
        );
        p.start(&[Stage::Normalize]);
        assert_eq!(p.skip_reason(Stage::Normalize), None);
    }

    #[test]
    fn state_path_ignores_the_pre_rename_settings() {
        let base = scratch_dir("none");
        write_state(&base, "hpdbs_app", "old");
        let path = state_path_in(&base);
        assert_eq!(path, base.join("dbsloc").join("state.json"));
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_remembered_subject_goes_to_the_front_once() {
        let mut state = DbslocAppState::default();
        remember_subject_in_state(&mut state, "D:/a", "sub-01");
        remember_subject_in_state(&mut state, "D:/a", "sub-02");
        remember_subject_in_state(&mut state, "D:/b", "sub-01");
        remember_subject_in_state(&mut state, "D:/a", "sub-01");
        let order: Vec<(&str, &str)> = state
            .recent_subjects
            .iter()
            .map(|r| (r.bids_dir.as_str(), r.subject.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![("D:/a", "sub-01"), ("D:/b", "sub-01"), ("D:/a", "sub-02")]
        );
    }

    #[test]
    fn recent_subjects_are_capped_and_drop_the_oldest() {
        let mut state = DbslocAppState::default();
        for i in 0..12 {
            remember_subject_in_state(&mut state, "D:/a", &format!("sub-{i:02}"));
        }
        assert_eq!(state.recent_subjects.len(), RECENT_LIMIT);
        assert_eq!(state.recent_subjects[0].subject, "sub-11");
        assert_eq!(state.recent_subjects[RECENT_LIMIT - 1].subject, "sub-04");
    }

    #[test]
    fn an_empty_folder_or_subject_is_not_remembered() {
        let mut state = DbslocAppState::default();
        remember_subject_in_state(&mut state, "  ", "sub-01");
        remember_subject_in_state(&mut state, "D:/a", "");
        assert!(state.recent_subjects.is_empty());
    }

    #[test]
    fn each_dataset_keeps_its_own_output_folder_across_a_round_trip() {
        let base = std::env::temp_dir().join(format!("dbsloc_app_out_{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        let out = base.join("results");
        let mut state = DbslocAppState {
            bids_dir: a.to_string_lossy().into_owned(),
            ..DbslocAppState::default()
        };
        assert_eq!(state.output_dir(), None);
        assert_eq!(
            state.derivatives_root(),
            Some(a.join("derivatives").join("leaddbs"))
        );
        state.set_output_dir(Some(&out.to_string_lossy()));
        let abs = dbsloc_pipeline::paths::absolute(&out);
        assert_eq!(state.output_dir(), Some(abs.clone()));
        assert_eq!(
            state.subject_derivatives_dir("sub-01"),
            Some(abs.join("sub-01"))
        );
        state.bids_dir = b.to_string_lossy().into_owned();
        assert_eq!(state.output_dir(), None);
        state.set_output_dir(Some(
            &dbsloc_pipeline::paths::default_derivatives_root(&b).to_string_lossy(),
        ));
        assert!(state
            .output_dirs
            .iter()
            .all(|d| Path::new(&d.bids_dir) != b));
        let back: DbslocAppState =
            serde_json::from_str(&serde_json::to_string(&state).expect("save")).expect("load");
        assert_eq!(back.output_dir_for(&a), Some(abs));
        state.bids_dir = a.to_string_lossy().into_owned();
        state.set_output_dir(None);
        assert_eq!(state.output_dir(), None);
        assert!(state.output_dirs.is_empty());
    }

    #[test]
    fn state_saved_before_the_landing_screen_still_loads() {
        let old = r#"{"bids_dir":"D:/a","recent_bids_dirs":["D:/a"],"selected_subject":"sub-01"}"#;
        let state: DbslocAppState = serde_json::from_str(old).expect("old state loads");
        assert_eq!(state.bids_dir, "D:/a");
        assert_eq!(state.selected_subject.as_deref(), Some("sub-01"));
        assert!(state.recent_subjects.is_empty());
        assert!(state.guided_default);
    }

    #[test]
    fn recent_subjects_and_the_guide_choice_survive_a_round_trip() {
        let mut state = DbslocAppState {
            guided_default: false,
            ..DbslocAppState::default()
        };
        remember_subject_in_state(&mut state, "D:/a", "sub-01");
        let back: DbslocAppState =
            serde_json::from_str(&state_to_json(&state)).expect("state round-trips");
        assert!(!back.guided_default);
        assert_eq!(back.recent_subjects, state.recent_subjects);
    }

    #[test]
    fn the_atlas_choice_and_atlases_directory_survive_a_round_trip() {
        let mut state = DbslocAppState::default();
        state.stage.leaddbs_atlases_dir = Some(PathBuf::from("D:/leaddbs"));
        state.scene.view = Some(dbsloc_viz::SceneViewState {
            atlas_id: Some("external:Morel".to_owned()),
            backdrop_id: Some("template-t2".to_owned()),
            ..Default::default()
        });
        let (back, complete) = parse_app_state(&state_to_json(&state));
        assert!(complete);
        assert_eq!(
            back.stage.leaddbs_atlases_dir,
            Some(PathBuf::from("D:/leaddbs"))
        );
        let view = back.scene.view.expect("view state kept");
        assert_eq!(view.atlas_id.as_deref(), Some("external:Morel"));
        assert_eq!(view.backdrop_id.as_deref(), Some("template-t2"));
        let (old, complete) = parse_app_state(
            r#"{"bids_dir": "D:/a", "stage": {"stop_on_error": true}, "scene": {"view": {"backdrop_id": "template-t1"}}}"#,
        );
        assert!(complete);
        assert_eq!(old.stage.leaddbs_atlases_dir, None);
        let view = old.scene.view.expect("old view state loads");
        assert_eq!(view.atlas_id, None);
        assert_eq!(view.backdrop_id.as_deref(), Some("template-t1"));
    }

    #[test]
    fn the_lead_dbs_import_flag_defaults_to_false_and_survives_a_round_trip() {
        let (old, complete) = parse_app_state(r#"{"bids_dir": "D:/a"}"#);
        assert!(complete);
        assert!(!old.leaddbs_import_offered);
        let state = DbslocAppState {
            leaddbs_import_offered: true,
            leaddbs_import_request: true,
            ..DbslocAppState::default()
        };
        let (back, complete) = parse_app_state(&state_to_json(&state));
        assert!(complete);
        assert!(back.leaddbs_import_offered);
        assert!(!back.leaddbs_import_request);
    }

    #[test]
    fn the_lead_dbs_connectomes_folder_survives_a_round_trip() {
        let mut state = DbslocAppState::default();
        state.stage.leaddbs_connectomes_dir = Some(PathBuf::from("D:/leaddbs/connectomes"));
        let (back, complete) = parse_app_state(&state_to_json(&state));
        assert!(complete);
        assert_eq!(
            back.stage.leaddbs_connectomes_dir,
            Some(PathBuf::from("D:/leaddbs/connectomes"))
        );
    }

    #[test]
    fn the_lead_dbs_import_screenshot_action_is_known() {
        assert_eq!(
            ScreenshotAction::parse("leaddbs-import"),
            ScreenshotAction::LeadDbsImport
        );
    }

    #[test]
    fn the_screenshot_action_defaults_to_the_landing_screen() {
        assert_eq!(ScreenshotAction::parse(""), ScreenshotAction::Landing);
        assert_eq!(
            ScreenshotAction::parse("guided:normalize"),
            ScreenshotAction::Guided
        );
        assert_eq!(ScreenshotAction::parse("guided"), ScreenshotAction::Guided);
        assert_eq!(ScreenshotAction::parse(" run "), ScreenshotAction::Run);
        assert_eq!(ScreenshotAction::parse("focus"), ScreenshotAction::Focus);
        assert_eq!(ScreenshotAction::parse("dock"), ScreenshotAction::Dock);
    }

    fn started(label: &str) -> RunEvent {
        RunEvent::StageStarted {
            stage: label.to_owned(),
        }
    }

    fn completed(label: &str) -> RunEvent {
        RunEvent::StageCompleted {
            stage: label.to_owned(),
            result: None,
        }
    }

    fn failed(label: &str) -> RunEvent {
        RunEvent::StageFailed {
            stage: label.to_owned(),
            message: "qc not approved".to_owned(),
        }
    }

    fn round_trip(dock: &DockState<TabKind>) -> DockState<TabKind> {
        let state = DbslocAppState {
            dock: Some(dock.clone()),
            ..DbslocAppState::default()
        };
        let (back, complete) = parse_app_state(&state_to_json(&state));
        assert!(complete);
        back.dock.expect("the dock survives")
    }

    #[test]
    fn a_never_drawn_dock_does_not_reset_the_saved_state() {
        let mut state = DbslocAppState {
            bids_dir: "D:/a".to_owned(),
            dock: Some(default_dock_state()),
            ..DbslocAppState::default()
        };
        remember_subject_in_state(&mut state, "D:/a", "sub-01");
        let text = state_to_json(&state);
        let (back, complete) = parse_app_state(&text);
        assert!(complete, "{text}");
        assert_eq!(back.bids_dir, "D:/a");
        assert_eq!(back.recent_subjects, state.recent_subjects);
        assert!(back.dock.is_some());
    }

    #[test]
    fn a_state_saved_with_null_dock_floats_still_loads() {
        let state = DbslocAppState {
            bids_dir: "D:/a".to_owned(),
            dock: Some(default_dock_state()),
            ..DbslocAppState::default()
        };
        let raw = serde_json::to_string(&state).expect("serialize");
        assert!(raw.contains("null"));
        let (back, complete) = parse_app_state(&raw);
        assert!(complete);
        assert_eq!(back.bids_dir, "D:/a");
    }

    #[test]
    fn an_unreadable_field_keeps_every_readable_one() {
        let text = r#"{
            "bids_dir": "D:/a",
            "recent_bids_dirs": ["D:/a"],
            "theme": "Sepia",
            "dock": {"surfaces": "broken"},
            "stage": {"run_coreg": false, "recon_method": "Unknown", "stop_on_error": true}
        }"#;
        let (back, complete) = parse_app_state(text);
        assert!(!complete);
        assert_eq!(back.bids_dir, "D:/a");
        assert_eq!(back.recent_bids_dirs, vec!["D:/a".to_owned()]);
        assert_eq!(back.theme, ThemeChoice::System);
        assert!(back.dock.is_none());
        assert!(!back.stage.run_coreg);
        assert!(back.stage.stop_on_error);
        assert_eq!(back.stage.recon_method, crate::panels::ReconMethod::Auto);
        let (garbage, complete) = parse_app_state("not json");
        assert!(!complete);
        assert!(garbage.bids_dir.is_empty());
    }

    #[test]
    fn the_state_file_is_replaced_in_one_step() {
        let base = scratch_dir("atomic");
        let path = base.join("state.json");
        write_atomically(&path, "{}").expect("first write");
        write_atomically(&path, "{\"bids_dir\": \"x\"}").expect("second write");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"bids_dir\": \"x\"}"
        );
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_abandoned_run_is_finalised_as_failed() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister, Stage::Normalize]);
        p.on_event(&started(Stage::Coregister.label()));
        p.abandon();
        assert_eq!(p.status_of(Stage::Coregister), Some(StageRunStatus::Failed));
        assert_eq!(
            p.status_of(Stage::Normalize),
            Some(StageRunStatus::Cancelled)
        );
        assert_eq!(p.summary(), Some("0 completed, 1 failed, 0 skipped"));
        assert!(p.any_failed());
    }

    #[test]
    fn a_pending_overwrite_belongs_to_one_subject() {
        let state = DbslocAppState::default();
        let req = StagePanel::default()
            .build_request(&state, &[Stage::Normalize])
            .expect("request");
        let pending = PendingOverwrite {
            req,
            stages: vec![Stage::Normalize],
            bids_dir: "D:/a".to_owned(),
            subject: Some("sub-01".to_owned()),
            batch: None,
        };
        assert!(pending.belongs_to("D:/a", Some("sub-01")));
        assert!(pending.belongs_to(" D:/a ", Some("sub-01")));
        assert!(!pending.belongs_to("D:/a", Some("sub-02")));
        assert!(!pending.belongs_to("D:/b", Some("sub-01")));
        assert!(!pending.belongs_to("D:/a", None));
    }

    #[test]
    fn a_batch_overwrite_question_survives_a_change_of_the_viewed_subject() {
        let state = DbslocAppState::default();
        let req = StagePanel::default()
            .build_request(&state, &[Stage::Normalize])
            .expect("request");
        let pending = PendingOverwrite {
            req,
            stages: vec![Stage::Normalize],
            bids_dir: "D:/a".to_owned(),
            subject: Some("sub-01".to_owned()),
            batch: Some(vec!["sub-01".to_owned(), "sub-02".to_owned()]),
        };
        assert!(pending.belongs_to("D:/a", Some("sub-02")));
        assert!(!pending.belongs_to("D:/b", Some("sub-01")));
    }

    fn batch_events(sub: &RunEvent, tail: &[RunEvent]) -> SubjectTally {
        let mut t = SubjectTally::default();
        t.on_event(sub);
        for ev in tail {
            t.on_event(ev);
        }
        t
    }

    fn end(ok: u32, failed: u32) -> RunEvent {
        RunEvent::PipelineCompleted {
            stages_ok: ok,
            stages_failed: failed,
            stages_skipped: 0,
        }
    }

    #[test]
    fn a_batch_subject_result_follows_its_events_and_the_review_mode() {
        let coreg = Stage::Coregister.label();
        let gated = [
            RunEvent::StageAwaitingQc {
                stage: coreg.to_owned(),
            },
            failed(coreg),
            end(1, 1),
        ];
        let t = batch_events(&completed(Stage::Preprocess.label()), &gated);
        let deferred = subject_result(
            "sub-01",
            &t,
            review_policy(ReviewMode::AfterAll),
            false,
            Duration::from_secs(2),
            false,
        );
        assert_eq!(deferred.phase, BatchPhase::AwaitingQc);
        assert_eq!(deferred.awaiting, Some(Stage::Coregister));
        assert_eq!((deferred.ok, deferred.failed), (1, 0));
        let rejected = subject_result(
            "sub-01",
            &t,
            review_policy(ReviewMode::DuringRun),
            false,
            Duration::ZERO,
            false,
        );
        assert_eq!(rejected.phase, BatchPhase::Failed);
        let done = batch_events(&completed(coreg), &[end(1, 0)]);
        let ok = subject_result(
            "sub-02",
            &done,
            ReviewPolicy::Off,
            false,
            Duration::ZERO,
            false,
        );
        assert_eq!(ok.phase, BatchPhase::Done);
        let cut = batch_events(&started(coreg), &[]);
        let crashed = subject_result(
            "sub-03",
            &cut,
            ReviewPolicy::Off,
            false,
            Duration::ZERO,
            true,
        );
        assert_eq!(crashed.phase, BatchPhase::Failed);
        assert!(crashed
            .message
            .is_some_and(|m| m.contains("without reporting")));
        let stopped = subject_result(
            "sub-03",
            &cut,
            ReviewPolicy::Off,
            true,
            Duration::ZERO,
            true,
        );
        assert_eq!(stopped.phase, BatchPhase::Cancelled);
    }

    #[test]
    fn a_deferred_gate_is_awaiting_review_not_rework() {
        let mut p = RunProgress::default();
        p.start_with(&[Stage::Coregister, Stage::Normalize], true);
        let coreg = Stage::Coregister.label();
        p.on_event(&started(coreg));
        p.on_event(&RunEvent::StageAwaitingQc {
            stage: coreg.to_owned(),
        });
        assert!(
            p.is_rejected_label(coreg),
            "the paired failure is not an error"
        );
        p.on_event(&failed(coreg));
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Deferred)
        );
        assert!(!p.any_failed());
        p.on_event(&end(0, 1));
        let summary = p.summary().unwrap_or_default().to_owned();
        assert!(
            summary.contains("0 failed") && summary.contains("1 awaiting review"),
            "{summary}"
        );
        assert!(p.progress_text().contains("1 awaiting review"));
        assert!(!summary.contains("rework"));
    }

    #[test]
    fn review_modes_map_to_the_pipeline_policies() {
        assert_eq!(
            review_policy(ReviewMode::DuringRun),
            ReviewPolicy::Interactive
        );
        assert_eq!(review_policy(ReviewMode::AfterAll), ReviewPolicy::Defer);
        assert_eq!(review_policy(ReviewMode::Off), ReviewPolicy::Off);
    }

    #[test]
    fn ticked_subjects_survive_a_round_trip_and_old_state_loads_without_them() {
        let mut state = DbslocAppState::default();
        state.checked_subjects.set("D:/a", "sub-02", true);
        state.checked_subjects.set("D:/a", "sub-01", true);
        state.stage.review_mode = ReviewMode::AfterAll;
        let back: DbslocAppState =
            serde_json::from_str(&state_to_json(&state)).expect("state round-trips");
        let listed = vec![
            "sub-01".to_owned(),
            "sub-02".to_owned(),
            "sub-03".to_owned(),
        ];
        assert_eq!(
            back.checked_subjects.ordered("D:/a", &listed),
            ["sub-01", "sub-02"]
        );
        assert_eq!(back.stage.review_mode, ReviewMode::AfterAll);
        let (old, _) =
            parse_app_state(r#"{"bids_dir": "D:/a", "stage": {"review_registrations": false}}"#);
        assert!(old.checked_subjects.subjects.is_empty());
        assert_eq!(old.stage.review_mode, ReviewMode::Off);
    }

    #[test]
    fn batch_seconds_read_as_minutes_once_long() {
        assert_eq!(format_seconds(42.4), "42 s");
        assert_eq!(format_seconds(125.0), "2 min 05 s");
    }

    #[test]
    fn a_review_request_waits_then_returns_to_running_and_completes() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister, Stage::Normalize]);
        let coreg = Stage::Coregister.label();
        p.on_event(&started(coreg));
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Running)
        );
        p.on_review_requested(Stage::Coregister);
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::AwaitingReview)
        );
        assert!(p.progress_text().contains("awaiting review"));
        assert_eq!(p.done_count(), 0);
        p.on_review_answered(Stage::Coregister);
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Running)
        );
        p.on_event(&completed(coreg));
        assert_eq!(p.status_of(Stage::Coregister), Some(StageRunStatus::Ok));
        assert!(!p.any_rejected());
    }

    #[test]
    fn a_late_stage_started_does_not_hide_a_pending_review() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister]);
        p.on_review_requested(Stage::Coregister);
        p.on_event(&started(Stage::Coregister.label()));
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::AwaitingReview)
        );
    }

    #[test]
    fn a_rejection_keeps_its_status_through_the_paired_failure() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister, Stage::Normalize]);
        let coreg = Stage::Coregister.label();
        p.on_event(&started(coreg));
        p.on_review_requested(Stage::Coregister);
        p.on_review_answered(Stage::Coregister);
        p.on_event(&RunEvent::StageAwaitingQc {
            stage: coreg.to_owned(),
        });
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Rejected)
        );
        assert!(p.is_rejected_label(coreg));
        p.on_event(&failed(coreg));
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Rejected)
        );
        assert!(!p.any_failed());
        p.on_event(&RunEvent::PipelineCompleted {
            stages_ok: 0,
            stages_failed: 1,
            stages_skipped: 0,
        });
        assert_eq!(
            p.status_of(Stage::Coregister),
            Some(StageRunStatus::Rejected)
        );
        assert_eq!(
            p.status_of(Stage::Normalize),
            Some(StageRunStatus::Cancelled)
        );
        assert!(p.progress_text().contains("1 needs rework"));
    }

    #[test]
    fn a_rejection_is_counted_as_rework_not_as_a_failure() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister, Stage::Normalize]);
        p.on_event(&RunEvent::StageAwaitingQc {
            stage: Stage::Coregister.label().to_owned(),
        });
        p.on_event(&RunEvent::PipelineCompleted {
            stages_ok: 0,
            stages_failed: 1,
            stages_skipped: 0,
        });
        assert_eq!(p.summary(), Some("1 needs rework"));
        assert_eq!(run_summary(2, 0, 1, 0), "2 completed, 0 failed, 1 skipped");
        assert_eq!(
            run_summary(2, 1, 0, 1),
            "2 completed, 1 failed, 0 skipped, 1 needs rework"
        );
    }

    #[test]
    fn a_failure_of_another_stage_is_still_a_failure() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Coregister, Stage::Normalize]);
        p.on_event(&RunEvent::StageAwaitingQc {
            stage: Stage::Coregister.label().to_owned(),
        });
        assert!(!p.is_rejected_label(Stage::Normalize.label()));
        p.on_event(&failed(Stage::Normalize.label()));
        assert_eq!(p.status_of(Stage::Normalize), Some(StageRunStatus::Failed));
    }

    #[test]
    fn an_unanswered_review_is_cancelled_when_the_run_ends() {
        let mut p = RunProgress::default();
        p.start(&[Stage::Normalize]);
        p.on_review_requested(Stage::Normalize);
        p.on_event(&RunEvent::PipelineCompleted {
            stages_ok: 0,
            stages_failed: 1,
            stages_skipped: 0,
        });
        assert_eq!(
            p.status_of(Stage::Normalize),
            Some(StageRunStatus::Cancelled)
        );
    }

    #[test]
    fn the_default_dock_holds_only_the_permanent_tabs() {
        let dock = default_dock_state();
        let output = dock.find_tab(&TabKind::Output).expect("output tab");
        let scene = dock.find_tab(&TabKind::Scene).expect("scene tab");
        assert_eq!(output.node_path(), scene.node_path());
        for tab in TabKind::ALL {
            assert_eq!(
                dock.find_tab(&tab).is_some(),
                !tab.is_transient(),
                "{tab:?}"
            );
        }
    }

    #[test]
    fn focusing_a_transient_tab_creates_it_beside_the_scene() {
        for tab in [
            TabKind::Review,
            TabKind::Refine,
            TabKind::Stimulation,
            TabKind::Group,
        ] {
            let mut dock = default_dock_state();
            focus_in_dock(&mut dock, tab);
            let scene = dock.find_tab(&TabKind::Scene).expect("scene tab");
            let found = dock.find_tab(&tab).expect("tab created");
            assert_eq!(scene.node_path(), found.node_path());
            focus_in_dock(&mut dock, tab);
            assert_eq!(dock.iter_all_tabs().filter(|(_, t)| **t == tab).count(), 1);
        }
    }

    #[test]
    fn transient_tabs_are_removed_from_a_restored_dock() {
        let mut dock = default_dock_state();
        focus_in_dock(&mut dock, TabKind::Review);
        focus_in_dock(&mut dock, TabKind::Group);
        let mut loaded = round_trip(&dock);
        remove_transient_tabs(&mut loaded);
        for tab in TabKind::ALL {
            assert_eq!(loaded.find_tab(&tab).is_some(), !tab.is_transient());
        }
    }

    #[test]
    fn only_idle_transient_tabs_can_be_closed() {
        let idle = TabBusy::default();
        for tab in [
            TabKind::Subjects,
            TabKind::Pipeline,
            TabKind::Output,
            TabKind::Scene,
        ] {
            assert!(!tab_closeable(tab, idle));
        }
        for tab in [
            TabKind::Review,
            TabKind::Refine,
            TabKind::Stimulation,
            TabKind::Group,
        ] {
            assert!(tab_closeable(tab, idle));
        }
        let busy = TabBusy {
            review: true,
            refine: true,
            stimulation: true,
            group: true,
        };
        assert!(!tab_closeable(TabKind::Review, busy));
        assert!(!tab_closeable(TabKind::Refine, busy));
        assert!(!tab_closeable(TabKind::Stimulation, busy));
        assert!(!tab_closeable(TabKind::Group, busy));
        assert!(tab_closeable(
            TabKind::Review,
            TabBusy {
                refine: true,
                ..idle
            }
        ));
    }

    #[test]
    fn recent_output_directories_are_deduplicated_and_capped() {
        let mut state = DbslocAppState::default();
        remember_output_dir_in_state(&mut state, "  ");
        assert!(state.recent_output_dirs.is_empty());
        for i in 0..(RECENT_LIMIT + 3) {
            remember_output_dir_in_state(&mut state, &format!("out-{i}"));
        }
        remember_output_dir_in_state(&mut state, "out-5");
        assert_eq!(state.recent_output_dirs.len(), RECENT_LIMIT);
        assert_eq!(state.recent_output_dirs[0], "out-5");
        assert_eq!(
            state
                .recent_output_dirs
                .iter()
                .filter(|p| *p == "out-5")
                .count(),
            1
        );
    }

    #[test]
    fn recent_output_directories_round_trip_and_old_state_loads() {
        let mut state = DbslocAppState::default();
        remember_output_dir_in_state(&mut state, "out-a");
        let json = state_to_json(&state);
        let back: DbslocAppState = serde_json::from_str(&json).expect("load");
        assert_eq!(back.recent_output_dirs, vec!["out-a".to_owned()]);
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("json");
        value
            .as_object_mut()
            .expect("object")
            .remove("recent_output_dirs");
        let old: DbslocAppState = serde_json::from_value(value).expect("old state");
        assert!(old.recent_output_dirs.is_empty());
    }

    #[test]
    fn a_saved_dock_without_the_review_tab_gets_it_appended() {
        let mut old: DockState<TabKind> = DockState::new(vec![TabKind::Output, TabKind::Scene]);
        old.main_surface_mut().split_left(
            NodeIndex::root(),
            0.3,
            vec![TabKind::Subjects, TabKind::Pipeline],
        );
        let mut loaded = round_trip(&old);
        assert!(loaded.find_tab(&TabKind::Review).is_none());
        ensure_tab_beside_scene(&mut loaded, TabKind::Review);
        let scene = loaded.find_tab(&TabKind::Scene).expect("scene tab");
        let review = loaded.find_tab(&TabKind::Review).expect("review tab");
        assert_eq!(scene.node_path(), review.node_path());
        ensure_tab_beside_scene(&mut loaded, TabKind::Review);
        assert_eq!(
            loaded
                .iter_all_tabs()
                .filter(|(_, t)| **t == TabKind::Review)
                .count(),
            1
        );
    }

    #[test]
    fn a_saved_dock_without_the_refine_tab_gets_it_appended_once() {
        let mut old: DockState<TabKind> = DockState::new(vec![TabKind::Output, TabKind::Scene]);
        old.main_surface_mut().split_left(
            NodeIndex::root(),
            0.3,
            vec![TabKind::Subjects, TabKind::Pipeline],
        );
        let mut loaded = round_trip(&old);
        assert!(loaded.find_tab(&TabKind::Refine).is_none());
        ensure_tab_beside_scene(&mut loaded, TabKind::Refine);
        let scene = loaded.find_tab(&TabKind::Scene).expect("scene tab");
        let refine = loaded.find_tab(&TabKind::Refine).expect("refine tab");
        assert_eq!(scene.node_path(), refine.node_path());
        ensure_tab_beside_scene(&mut loaded, TabKind::Refine);
        assert_eq!(
            loaded
                .iter_all_tabs()
                .filter(|(_, t)| **t == TabKind::Refine)
                .count(),
            1
        );
    }

    #[test]
    fn quitting_asks_while_a_run_unsaved_corrections_or_an_import_write_exist() {
        assert!(!quit_needs_confirm(false, false, false, false));
        assert!(quit_needs_confirm(true, false, false, false));
        assert!(quit_needs_confirm(false, true, false, false));
        assert!(quit_needs_confirm(false, false, true, false));
        assert!(quit_needs_confirm(true, true, false, false));
        assert!(!quit_needs_confirm(true, true, false, true));
        assert!(!quit_needs_confirm(false, true, false, true));
        assert!(!quit_needs_confirm(false, false, true, true));
    }

    #[test]
    fn a_repeated_error_is_kept_once() {
        let mut errors = Vec::new();
        for _ in 0..3 {
            add_error(
                &mut errors,
                "Cannot start the run".to_owned(),
                RUN_ALREADY_ACTIVE.to_owned(),
            );
        }
        assert_eq!(errors.len(), 1);
        assert!(add_error(
            &mut errors,
            "Cannot start the run".to_owned(),
            BATCH_ACTIVE.to_owned()
        ));
        assert!(!add_error(
            &mut errors,
            "Cannot start the run".to_owned(),
            RUN_ALREADY_ACTIVE.to_owned()
        ));
        assert_eq!(errors.len(), 2);
        assert_eq!(group_errors(&errors).len(), 2);
    }

    #[test]
    fn importing_is_blocked_while_another_job_writes_into_a_subject() {
        let none = ImportBlockState::default();
        assert_eq!(import_block(none), None);
        let cases = [
            (
                ImportBlockState {
                    running: true,
                    ..none
                },
                IMPORT_LOCKED_RUN,
            ),
            (
                ImportBlockState {
                    batch_active: true,
                    ..none
                },
                IMPORT_LOCKED_BATCH,
            ),
            (
                ImportBlockState {
                    review_open: true,
                    ..none
                },
                IMPORT_LOCKED_REVIEW,
            ),
            (
                ImportBlockState {
                    review_finishing: true,
                    ..none
                },
                IMPORT_LOCKED_REVIEW_FINISHING,
            ),
            (
                ImportBlockState {
                    refine_open: true,
                    ..none
                },
                IMPORT_LOCKED_REFINE,
            ),
            (
                ImportBlockState {
                    refine_saving: true,
                    ..none
                },
                IMPORT_LOCKED_REFINE_SAVING,
            ),
            (
                ImportBlockState {
                    group_calculating: true,
                    ..none
                },
                IMPORT_LOCKED_GROUP,
            ),
        ];
        for (state, reason) in cases {
            assert_eq!(import_block(state), Some(reason), "{state:?}");
        }
        assert_eq!(
            import_block(ImportBlockState {
                running: true,
                batch_active: true,
                ..none
            }),
            Some(IMPORT_LOCKED_BATCH)
        );
        assert_eq!(
            import_block(ImportBlockState {
                review_open: true,
                review_finishing: true,
                ..none
            }),
            Some(IMPORT_LOCKED_REVIEW_FINISHING)
        );
        assert_eq!(
            import_block(ImportBlockState {
                refine_open: true,
                refine_saving: true,
                ..none
            }),
            Some(IMPORT_LOCKED_REFINE_SAVING)
        );
    }

    #[test]
    fn a_busy_import_holds_back_every_run() {
        assert_eq!(run_wait_for(RunWaitState::default()), None);
        assert_eq!(
            run_wait_for(RunWaitState {
                importing: true,
                ..RunWaitState::default()
            }),
            Some(RUN_WAIT_IMPORT)
        );
        assert_eq!(
            run_wait_for(RunWaitState {
                refine_dirty: true,
                importing: true,
                ..RunWaitState::default()
            }),
            Some(RUN_WAIT_REFINE_DIRTY)
        );

        let ctx = egui::Context::default();
        let mut w = ImportWizard::new("");
        let (release, wait_for_release) = mpsc::channel::<()>();
        w.spawn_import(
            vec!["01".to_owned()],
            false,
            move |_, _| {
                let _ = wait_for_release.recv();
                Vec::new()
            },
            None,
        );
        let state = |w: &ImportWizard| RunWaitState {
            importing: w.importing(),
            ..RunWaitState::default()
        };
        assert_eq!(run_wait_for(state(&w)), Some(RUN_WAIT_IMPORT));
        drop(release);
        let start = Instant::now();
        while w.importing() && start.elapsed() < Duration::from_secs(10) {
            w.poll(&ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(run_wait_for(state(&w)), None);

        let (release, wait_for_release) = mpsc::channel::<()>();
        w.spawn_conversion(
            PathBuf::from("dicom"),
            None,
            move |_| {
                let _ = wait_for_release.recv();
                Ok(Vec::new())
            },
            None,
        );
        assert!(w.busy());
        assert_eq!(run_wait_for(state(&w)), None);
        drop(release);
    }

    #[test]
    fn a_prepared_lead_is_kept_only_for_the_subject_it_was_opened_for() {
        let owner = RefineOwner {
            bids_dir: PathBuf::from("data"),
            subject: "01".to_owned(),
        };
        assert!(refine_owner_matches(&owner, "data", Some("sub-01")));
        assert!(refine_owner_matches(&owner, "data", Some("01")));
        assert!(!refine_owner_matches(&owner, "data", Some("sub-02")));
        assert!(!refine_owner_matches(&owner, "other", Some("sub-01")));
        assert!(!refine_owner_matches(&owner, "data", None));
    }

    #[test]
    fn a_followed_lead_that_arrives_late_is_dropped_and_retried_never_opened() {
        let follow = PreparedRefineState {
            follow: true,
            current: true,
            awaited: true,
            ..PreparedRefineState::default()
        };
        assert_eq!(
            prepared_refine_action(follow),
            PreparedRefineAction::OpenQuietly
        );
        for stale in [
            PreparedRefineState {
                current: false,
                ..follow
            },
            PreparedRefineState {
                awaited: false,
                ..follow
            },
        ] {
            assert_eq!(
                prepared_refine_action(stale),
                PreparedRefineAction::DropAndRetry
            );
        }
        for job in [
            PreparedRefineState {
                running: true,
                ..follow
            },
            PreparedRefineState {
                importing: true,
                ..follow
            },
            PreparedRefineState {
                running: true,
                job_since_start: true,
                ..follow
            },
        ] {
            assert_eq!(
                prepared_refine_action(job),
                PreparedRefineAction::RetryAfterJob
            );
        }
        assert_eq!(
            prepared_refine_action(PreparedRefineState {
                job_since_start: true,
                ..follow
            }),
            PreparedRefineAction::DropAndRetry
        );
        let explicit = PreparedRefineState {
            follow: false,
            ..follow
        };
        assert_eq!(prepared_refine_action(explicit), PreparedRefineAction::Open);
        assert_eq!(
            prepared_refine_action(PreparedRefineState {
                current: false,
                ..explicit
            }),
            PreparedRefineAction::Drop
        );
        assert_eq!(
            prepared_refine_action(PreparedRefineState {
                running: true,
                ..explicit
            }),
            PreparedRefineAction::RefuseRun
        );
    }

    #[test]
    fn a_run_or_import_that_starts_and_ends_while_a_lead_opens_is_still_noticed() {
        let started = 4;
        let (importing, epoch) = importing_edge(false, true, started);
        assert!(importing);
        assert_ne!(epoch, started);
        let (importing, after) = importing_edge(importing, false, epoch);
        assert!(!importing);
        assert_eq!(after, epoch);
        let (_, idle) = importing_edge(false, false, started);
        assert_eq!(idle, started);
        let (_, still) = importing_edge(true, true, started);
        assert_eq!(still, started);
        assert_eq!(
            prepared_refine_action(PreparedRefineState {
                follow: true,
                current: true,
                awaited: true,
                job_since_start: after != started,
                ..PreparedRefineState::default()
            }),
            PreparedRefineAction::DropAndRetry
        );
    }

    #[test]
    fn refine_follows_only_while_no_job_and_no_run_holds_it() {
        assert!(!refine_follow_busy(RefineFollowJobs::default()));
        let each = [
            RefineFollowJobs {
                run_active: true,
                ..RefineFollowJobs::default()
            },
            RefineFollowJobs {
                preparing: true,
                ..RefineFollowJobs::default()
            },
            RefineFollowJobs {
                saving: true,
                ..RefineFollowJobs::default()
            },
            RefineFollowJobs {
                review_preparing: true,
                ..RefineFollowJobs::default()
            },
            RefineFollowJobs {
                review_finishing: true,
                ..RefineFollowJobs::default()
            },
            RefineFollowJobs {
                importing: true,
                ..RefineFollowJobs::default()
            },
        ];
        for jobs in each {
            assert!(refine_follow_busy(jobs), "{jobs:?}");
        }
    }

    #[test]
    fn a_followed_lead_being_opened_never_blocks_a_run_or_an_import() {
        let wait = |preparing, follow| {
            run_wait_for(RunWaitState {
                refine_preparing: refine_prepare_blocks(preparing, follow),
                ..RunWaitState::default()
            })
        };
        assert_eq!(wait(true, true), None);
        assert_eq!(wait(false, false), None);
        assert_eq!(wait(true, false), Some(RUN_WAIT_REFINE_PREPARING));
        let import = |preparing, follow| {
            import_block(ImportBlockState {
                refine_open: refine_prepare_blocks(preparing, follow),
                ..ImportBlockState::default()
            })
        };
        assert_eq!(import(true, true), None);
        assert_eq!(import(true, false), Some(IMPORT_LOCKED_REFINE));
    }

    #[test]
    fn ctrl_six_focuses_the_refine_tab() {
        assert_eq!(TabKind::ALL[5], TabKind::Refine);
        assert_eq!(TabKind::Refine.title(), "Refine localization");
    }

    #[test]
    fn the_refine_screenshot_action_is_parsed_and_is_not_a_review_shot() {
        assert_eq!(ScreenshotAction::parse("refine"), ScreenshotAction::Refine);
        assert_eq!(
            ScreenshotAction::parse(" refine "),
            ScreenshotAction::Refine
        );
        assert_eq!(ScreenshotAction::Refine.review_shot(), None);
        assert!(screenshot_mode_from(None, Some("refine")));
    }

    #[test]
    fn the_dock_with_the_review_tab_survives_a_round_trip() {
        let mut dock = default_dock_state();
        focus_in_dock(&mut dock, TabKind::Review);
        let back = round_trip(&dock);
        assert!(back.find_tab(&TabKind::Review).is_some());
    }

    #[test]
    fn tab_kinds_serialize_by_name() {
        for tab in TabKind::ALL {
            let json = serde_json::to_string(&tab).expect("serialize");
            let back: TabKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, tab);
        }
        assert_eq!(
            serde_json::to_string(&TabKind::Review).expect("serialize"),
            "\"Review\""
        );
    }
}
