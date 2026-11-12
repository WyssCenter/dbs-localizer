// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The *Group* tab: a Lead Group style cohort record (`specs/14`) with its patients, groups,
//! clinical variables and stimulation, *Calculate* on a worker, and the group 3D view.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use dbsloc_pipeline::atlas_catalog::{bundled_and_external_atlas_sets, default_atlas};
use dbsloc_pipeline::group::calc::{
    run_calculate_job, CalcEvent, CalcOptions, CalcReport, FlipTransformChoice,
};
use dbsloc_pipeline::group::clinical::{
    delimiter_for, import_table, write_clinical_export, ImportOptions,
};
use dbsloc_pipeline::group::exports::{write_stats_exports, ExportOptions};
use dbsloc_pipeline::group::record::{default_color, subject_id, StimulationRef};
use dbsloc_pipeline::group::sweetspot::Normalisation;
use dbsloc_pipeline::group::view::{
    write_group_view, ColourBy, ShowFlags, ViewOptions, ViewReport,
};
use dbsloc_pipeline::group::{
    create_group, locate, open_group, save_group, GroupRecord, Note, Opened,
};
use dbsloc_pipeline::options::RunOptions;
use dbsloc_pipeline::vat::settings::{
    list_labels, stimulation_json_path, stimulations_dir, LabelState, ModelKind,
};
use eframe::egui;

use crate::connectome_cache_ui::CacheRun;
use crate::group_explorers::{
    self as explorers, AnalysisDone, AnalysisFail, GroupLayer, Kind, Request,
};
use crate::panels::{em, palette, popout_window, PopoutBar};
use crate::stimulation_panel::StimFocus;

pub const GROUP_PLACEHOLDER: &str = "Group analysis: open a group file or create a group.";

/// Groups kept in the recent list.
pub const RECENT_GROUP_LIMIT: usize = 8;

const POPOUT_SALT: &str = "dbsloc_group_popout";
const POPOUT_TITLE: &str = "Group - Wyss DBS Localizer";
const POPOUT_NAME: &str = "the group panel";

/// `path` first in `list`, without a duplicate, at most [`RECENT_GROUP_LIMIT`] entries.
pub fn remember_group(list: &mut Vec<String>, path: &str) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    list.retain(|p| p != path);
    list.insert(0, path.to_owned());
    list.truncate(RECENT_GROUP_LIMIT);
}

/// Subject ids separated by spaces, commas, semicolons or line breaks.
pub fn parse_ids(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in text
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let id = id.strip_prefix("sub-").unwrap_or(id).to_owned();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The normalisation a Lead Group `normregpopup` code stands for: 1 none, 2 z-score, 3 van Albada.
pub fn normalisation_of(code: u32) -> Normalisation {
    match code {
        2 => Normalisation::ZScore,
        3 => Normalisation::VanAlbada,
        _ => Normalisation::None,
    }
}

/// The code of a normalisation, the inverse of [`normalisation_of`].
pub fn normalisation_code(n: Normalisation) -> u32 {
    match n {
        Normalisation::None => 1,
        Normalisation::ZScore => 2,
        Normalisation::VanAlbada => 3,
    }
}

fn normalisation_label(n: Normalisation) -> &'static str {
    match n {
        Normalisation::None => "None",
        Normalisation::ZScore => "z-score",
        Normalisation::VanAlbada => "van Albada 2007",
    }
}

/// What the contacts are coloured by: the first `view.colour_by` name when contact colouring is on and the group has that variable.
pub fn colour_choice(record: &GroupRecord) -> ColourBy {
    if !record.view.colour_contacts {
        return ColourBy::Group;
    }
    match record.view.colour_by.first() {
        Some(name) if record.clinical.iter().any(|v| &v.name == name) => {
            ColourBy::Clinical(name.clone())
        }
        _ => ColourBy::Group,
    }
}

/// The subjects the add button adds and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddChoice {
    pub ids: Vec<String>,
    pub label: String,
    pub ticked: bool,
}

/// Ticked subjects win over the highlighted one; ids come without the `sub-` prefix.
pub fn add_choice(ticked: &[String], selected: Option<&str>) -> AddChoice {
    let strip = |s: &str| s.strip_prefix("sub-").unwrap_or(s).to_owned();
    let mut ids: Vec<String> = Vec::new();
    for id in ticked.iter().map(|t| strip(t)) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if !ids.is_empty() {
        let label = format!("Add ticked subjects ({})", ids.len());
        return AddChoice {
            ids,
            label,
            ticked: true,
        };
    }
    match selected.map(strip) {
        Some(id) => AddChoice {
            label: format!("Add sub-{id}"),
            ids: vec![id],
            ticked: false,
        },
        None => AddChoice {
            ids,
            label: "Add ticked subjects".to_owned(),
            ticked: true,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatientStatus {
    NoReconstruction,
    NoStimulation,
    Ready(Option<String>),
    Computed,
}

impl PatientStatus {
    fn runnable(&self) -> bool {
        matches!(self, PatientStatus::Ready(_) | PatientStatus::Computed)
    }

    fn text(&self) -> &'static str {
        match self {
            PatientStatus::NoReconstruction => "No reconstruction",
            PatientStatus::NoStimulation => "No stimulation",
            PatientStatus::Ready(_) => "Ready",
            PatientStatus::Computed => "Computed",
        }
    }

    fn tip(&self, source: Option<&str>) -> String {
        match self {
            PatientStatus::NoReconstruction => {
                "The patient has no reconstruction file; run the pipeline for it first".to_owned()
            }
            PatientStatus::NoStimulation => match source {
                Some(l) => format!("Neither group settings nor a stimulation labelled {l:?} exist for this patient"),
                None => "No group settings for this patient; choose a source under Stimulation, Settings from, or create the settings in the stimulation editor".to_owned(),
            },
            PatientStatus::Ready(Some(l)) => {
                format!("Will be computed; the settings are a copy of {l:?}")
            }
            PatientStatus::Ready(None) => "Will be computed with the group settings".to_owned(),
            PatientStatus::Computed => {
                "The volume of tissue activated exists in template space".to_owned()
            }
        }
    }

    fn colour(&self, pal: &crate::app::tokens::Palette) -> egui::Color32 {
        match self {
            PatientStatus::NoReconstruction | PatientStatus::NoStimulation => pal.warn,
            PatientStatus::Ready(_) => pal.text_muted,
            PatientStatus::Computed => pal.ok,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanKey {
    patients: Vec<(String, Option<PathBuf>)>,
    label: String,
    source: Option<String>,
}

struct ScanResult {
    key: ScanKey,
    statuses: HashMap<String, PatientStatus>,
    labels: Vec<(String, usize)>,
}

fn reco_file(folder: &Path, subject: &str) -> PathBuf {
    folder
        .join("reconstruction")
        .join(format!("{subject}_desc-reconstruction.json"))
}

fn vta_computed(folder: &Path, sid: &str, label: &str) -> bool {
    let Ok(spaces) = std::fs::read_dir(stimulations_dir(folder)) else {
        return false;
    };
    spaces
        .flatten()
        .filter(|e| e.file_name() != "native")
        .any(|e| {
            e.path()
                .join(label)
                .join(format!("sub-{sid}_desc-vta.json"))
                .is_file()
        })
}

fn scan_patients(key: &ScanKey) -> ScanResult {
    let mut statuses = HashMap::new();
    let mut labels: Vec<(String, usize)> = Vec::new();
    for (subject, folder) in &key.patients {
        let sid = subject_id(subject);
        let status = match folder {
            Some(folder) if reco_file(folder, subject).is_file() => {
                for e in list_labels(folder, sid) {
                    if e.state != LabelState::Canonical {
                        continue;
                    }
                    match labels.iter_mut().find(|(l, _)| *l == e.label) {
                        Some(entry) => entry.1 += 1,
                        None => labels.push((e.label, 1)),
                    }
                }
                let source = key
                    .source
                    .as_deref()
                    .filter(|l| stimulation_json_path(folder, sid, l).is_file());
                if vta_computed(folder, sid, &key.label) {
                    PatientStatus::Computed
                } else if let Some(l) = source {
                    PatientStatus::Ready(Some(l.to_owned()))
                } else if stimulation_json_path(folder, sid, &key.label).is_file() {
                    PatientStatus::Ready(None)
                } else {
                    PatientStatus::NoStimulation
                }
            }
            _ => PatientStatus::NoReconstruction,
        };
        statuses.insert(subject.clone(), status);
    }
    labels.sort();
    ScanResult {
        key: key.clone(),
        statuses,
        labels,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn status_line<'a>(total: usize, statuses: impl Iterator<Item = &'a PatientStatus>) -> String {
    let (mut ready, mut computed, mut no_stim, mut no_reco) = (0, 0, 0, 0);
    for st in statuses {
        match st {
            PatientStatus::Ready(_) => ready += 1,
            PatientStatus::Computed => computed += 1,
            PatientStatus::NoStimulation => no_stim += 1,
            PatientStatus::NoReconstruction => no_reco += 1,
        }
    }
    let mut parts = vec![plural(total, "patient", "patients")];
    for (n, text) in [
        (ready, "ready"),
        (computed, "computed"),
        (no_stim, "without stimulation"),
        (no_reco, "without reconstruction"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {text}"));
        }
    }
    parts.join(" · ")
}

fn compute_summary(runnable: usize, source: Option<&str>, atlas: &str, mirror: bool) -> String {
    if runnable == 0 {
        return "Nothing to compute: no patient is ready".to_owned();
    }
    let from = match source {
        Some(l) => format!("{l:?}"),
        None => "group settings".to_owned(),
    };
    format!(
        "For {}: VTA from {from} → atlas overlap with {atlas} → contact distances{}",
        plural(runnable, "patient", "patients"),
        if mirror { " → mirrored volumes" } else { "" }
    )
}

fn source_label(label: &str, labels: &[(String, usize)], total: usize) -> String {
    match labels.iter().find(|(l, _)| l == label) {
        Some((_, n)) => format!("{label} ({n} of {total})"),
        None => label.to_owned(),
    }
}

fn default_source(
    labels: &[(String, usize)],
    statuses: &HashMap<String, PatientStatus>,
) -> Option<String> {
    let none_has_settings = !statuses.is_empty()
        && statuses.values().all(|st| {
            matches!(
                st,
                PatientStatus::NoReconstruction | PatientStatus::NoStimulation
            )
        });
    match labels {
        [(l, _)] if none_has_settings => Some(l.clone()),
        _ => None,
    }
}

#[derive(Default)]
struct AtlasList {
    entries: Vec<(String, String)>,
    default_name: Option<String>,
}

enum AtlasState {
    Unloaded,
    Loading(mpsc::Receiver<AtlasList>),
    Ready(AtlasList),
}

fn atlas_text(list: Option<&AtlasList>, current: Option<&str>) -> String {
    match current {
        None => match list.and_then(|l| l.default_name.as_deref()) {
            Some(n) => format!("Default ({n})"),
            None => "Default".to_owned(),
        },
        Some(id) => list
            .and_then(|l| l.entries.iter().find(|(i, _)| i == id))
            .map_or_else(|| id.to_owned(), |(_, n)| n.clone()),
    }
}

fn atlas_summary_name(list: Option<&AtlasList>, current: Option<&str>) -> String {
    match current {
        None => list
            .and_then(|l| l.default_name.clone())
            .unwrap_or_else(|| "default atlas".to_owned()),
        Some(_) => atlas_text(list, current),
    }
}

/// What the application tells the tab each frame.
#[derive(Debug, Clone, Default)]
pub struct GroupEnv {
    pub run_active: bool,
    pub dataset_root: Option<PathBuf>,
    pub derivatives_root: Option<PathBuf>,
    pub selected_subject: Option<String>,
    pub ticked_subjects: Vec<String>,
    pub template: Option<String>,
    pub atlases_dir: Option<PathBuf>,
    pub recent: Vec<String>,
    pub connectomes_dir: Option<PathBuf>,
    pub last_connectomes: BTreeMap<String, String>,
}

/// The view options of a group scene for the record's colouring, mirroring and display flags.
pub fn view_options(record: &GroupRecord, env: &GroupEnv, plain_flip: bool) -> ViewOptions {
    let mut o = ViewOptions::new();
    o.colour_by = colour_choice(record);
    o.normalisation = normalisation_of(record.view.normalisation);
    o.mirror = record.view.mirror_sides;
    o.template = env.template.clone().filter(|t| !t.trim().is_empty());
    o.leaddbs_atlases_dir = env.atlases_dir.clone();
    if plain_flip {
        o.flip_transform = FlipTransformChoice::PlainFlip;
    }
    let clinical = matches!(o.colour_by, ColourBy::Clinical(_));
    o.show = Some(ShowFlags {
        active_contacts: record.view.show_active,
        passive_contacts: record.view.show_passive,
        highlight_active: record.view.highlight_active,
        vtas: true,
        point_cloud: clinical,
        isovolume: false,
    });
    o
}

/// The options of a *Calculate* job.
pub fn calc_options(
    run: RunOptions,
    record: &GroupRecord,
    plain_flip: bool,
    copy_from: &str,
) -> CalcOptions {
    let mut o = CalcOptions::new(run);
    o.mirror = record.view.mirror_sides;
    o.flip_transform = if plain_flip {
        FlipTransformChoice::PlainFlip
    } else {
        FlipTransformChoice::Auto
    };
    let copy = copy_from.trim();
    o.copy_settings_from = (!copy.is_empty()).then(|| copy.to_owned());
    o.sync_settings = o.copy_settings_from.is_some();
    o
}

/// How the scene should be drawn right now: display flags and the highlighted patient or group.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupLook {
    pub show_active: bool,
    pub show_passive: bool,
    pub highlight_active: bool,
    pub selected_patient: Option<String>,
    pub selected_group: Option<u32>,
}

/// What the tab asks the application to do.
#[derive(Debug, Default)]
pub struct GroupOutput {
    pub log: Vec<String>,
    pub errors: Vec<(String, String)>,
    pub focus: bool,
    pub calc: bool,
    pub show_scene: Option<PathBuf>,
    pub remember: Option<String>,
    pub look: Option<GroupLook>,
    pub edit_stimulation: Option<StimFocus>,
    pub layers: Vec<GroupLayer>,
    pub remove_layers: Vec<String>,
    pub remember_connectome: Option<(String, String)>,
}

pub fn edit_focus(subject: &str, source: Option<&str>, group_label: &str) -> StimFocus {
    match source.map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) => StimFocus {
            subject: subject.to_owned(),
            label: label.to_owned(),
            group_label: false,
        },
        None => StimFocus {
            subject: subject.to_owned(),
            label: group_label.to_owned(),
            group_label: true,
        },
    }
}

pub fn edit_block_reason(
    has_reco: bool,
    in_current_dataset: bool,
    busy: bool,
) -> Option<&'static str> {
    if busy {
        Some("Wait for the running job to finish")
    } else if !has_reco {
        Some("The patient has no reconstruction yet")
    } else if !in_current_dataset {
        Some("Open the patient's dataset as the input directory first")
    } else {
        None
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

impl GroupOutput {
    pub fn merge(&mut self, other: GroupOutput) {
        if other.edit_stimulation.is_some() {
            self.edit_stimulation = other.edit_stimulation;
        }
        self.log.extend(other.log);
        self.errors.extend(other.errors);
        self.focus |= other.focus;
        self.calc |= other.calc;
        if other.show_scene.is_some() {
            self.show_scene = other.show_scene;
        }
        if other.remember.is_some() {
            self.remember = other.remember;
        }
        if other.look.is_some() {
            self.look = other.look;
        }
        self.layers.extend(other.layers);
        self.remove_layers.extend(other.remove_layers);
        if other.remember_connectome.is_some() {
            self.remember_connectome = other.remember_connectome;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Calculate,
    View,
    Export,
    Analysis,
    Layers,
}

impl JobKind {
    fn label(self) -> &'static str {
        match self {
            JobKind::Calculate => "Computing VTAs",
            JobKind::View => "Preparing the 3D view",
            JobKind::Export => "Exporting tables",
            JobKind::Analysis => "Analysis",
            JobKind::Layers => "Loading the result",
        }
    }
}

enum JobMsg {
    Log(String),
    Progress {
        done: usize,
        total: usize,
        subject: String,
    },
    Calc(Result<Box<CalcReport>, String>),
    View(Result<Box<ViewReport>, String>),
    Export(Result<Vec<PathBuf>, String>),
    Analysis(Result<Box<AnalysisDone>, AnalysisFail>),
    Layers(Result<Vec<GroupLayer>, String>),
}

struct Job {
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<JobMsg>,
    started: Instant,
    done: usize,
    total: usize,
    subject: String,
}

type PickRx = mpsc::Receiver<Option<PathBuf>>;

#[derive(Default)]
struct Picks {
    open: Option<PickRx>,
    dataset: Option<PickRx>,
    table: Option<PickRx>,
}

fn spawn_pick(
    ctx: &egui::Context,
    slot: &mut Option<PickRx>,
    dialog: impl FnOnce() -> Option<PathBuf> + Send + 'static,
) {
    if slot.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    *slot = Some(rx);
    let ctx = ctx.clone();
    thread::spawn(move || {
        let _ = tx.send(dialog());
        ctx.request_repaint();
    });
}

fn take_pick(slot: &mut Option<PickRx>) -> Option<PathBuf> {
    let rx = slot.as_ref()?;
    match rx.try_recv() {
        Ok(v) => {
            *slot = None;
            v
        }
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            *slot = None;
            None
        }
    }
}

struct NewForm {
    dataset_root: String,
    title: String,
    guid: String,
}

impl Default for NewForm {
    fn default() -> Self {
        NewForm {
            dataset_root: String::new(),
            title: String::new(),
            guid: now_guid(),
        }
    }
}

fn now_guid() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    crate::stimulation_panel::timestamp_label(secs)
}

struct Session {
    json_path: PathBuf,
    folder: PathBuf,
    record: GroupRecord,
    notes: Vec<Note>,
    dirty: bool,
    job: Option<Job>,
    selected: Option<usize>,
    selected_group: Option<u32>,
    has_reco: Vec<bool>,
    scan: Option<ScanResult>,
    scan_rx: Option<mpsc::Receiver<ScanResult>>,
    rescan: bool,
    source_decided: bool,
    atlases: AtlasState,
    add_ids: String,
    replace_clinical: bool,
    plain_flip: bool,
    copy_from: String,
    status: Option<String>,
    export_dir: Option<PathBuf>,
    explorers: explorers::ExplorerState,
}

enum Phase {
    Idle,
    Opening(mpsc::Receiver<Result<Box<Opened>, String>>),
    Open(Box<Session>),
}

/// The *Group* tab.
pub struct GroupTab {
    phase: Phase,
    popped_out: bool,
    notice: Option<String>,
    form: NewForm,
    picks: Picks,
    looked: Option<GroupLook>,
    cache: dbsloc_pipeline::connectome_cache::CacheSetting,
    calc_workers: Arc<AtomicUsize>,
}

impl Default for GroupTab {
    fn default() -> Self {
        GroupTab {
            phase: Phase::Idle,
            popped_out: false,
            notice: None,
            form: NewForm::default(),
            picks: Picks::default(),
            looked: None,
            cache: dbsloc_pipeline::connectome_cache::CacheSetting::default(),
            calc_workers: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct WorkerGuard(Arc<AtomicUsize>);

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn spawn_counted<F>(
    workers: &Arc<AtomicUsize>,
    repaint: Option<egui::Context>,
    job: F,
) -> thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    workers.fetch_add(1, Ordering::SeqCst);
    let guard = WorkerGuard(Arc::clone(workers));
    thread::spawn(move || {
        let guard = guard;
        job();
        drop(guard);
        if let Some(ctx) = repaint {
            ctx.request_repaint();
        }
    })
}

fn reco_exists(record: &GroupRecord) -> Vec<bool> {
    record
        .patients
        .iter()
        .map(|p| {
            record.patient_folder(p).is_some_and(|dir| {
                dir.join("reconstruction")
                    .join(format!("{}_desc-reconstruction.json", p.subject))
                    .is_file()
            })
        })
        .collect()
}

impl Session {
    fn new(opened: Opened) -> Session {
        let has_reco = reco_exists(&opened.record);
        let mut notes = opened.notes;
        notes.shrink_to_fit();
        Session {
            json_path: opened.json_path,
            folder: opened.folder,
            record: opened.record,
            notes,
            dirty: false,
            job: None,
            selected: None,
            selected_group: None,
            has_reco,
            scan: None,
            scan_rx: None,
            rescan: true,
            source_decided: false,
            atlases: AtlasState::Unloaded,
            add_ids: String::new(),
            replace_clinical: false,
            plain_flip: false,
            copy_from: String::new(),
            status: None,
            export_dir: None,
            explorers: explorers::ExplorerState::default(),
        }
    }

    fn look(&self) -> GroupLook {
        GroupLook {
            show_active: self.record.view.show_active,
            show_passive: self.record.view.show_passive,
            highlight_active: self.record.view.highlight_active,
            selected_patient: self
                .selected
                .and_then(|i| self.record.patients.get(i))
                .map(|p| p.subject.clone()),
            selected_group: self.selected_group,
        }
    }

    fn scan_key(&self) -> ScanKey {
        ScanKey {
            patients: self
                .record
                .patients
                .iter()
                .map(|p| (p.subject.clone(), self.record.patient_folder(p)))
                .collect(),
            label: self.record.stim_label(),
            source: (!self.copy_from.trim().is_empty()).then(|| self.copy_from.trim().to_owned()),
        }
    }

    fn drive_scan(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.scan_rx {
            match rx.try_recv() {
                Ok(result) => {
                    self.scan_rx = None;
                    if !self.source_decided && !result.key.patients.is_empty() {
                        self.source_decided = true;
                        if result.key.source.is_none() {
                            if let Some(l) = default_source(&result.labels, &result.statuses) {
                                self.copy_from = l;
                            }
                        }
                    }
                    self.scan = Some(result);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.scan_rx = None,
            }
        }
        let key = self.scan_key();
        if !self.rescan && self.scan.as_ref().map(|r| &r.key) == Some(&key) {
            return;
        }
        self.rescan = false;
        let (tx, rx) = mpsc::channel();
        self.scan_rx = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(scan_patients(&key));
            ctx.request_repaint();
        });
    }

    fn drive_atlases(&mut self, ctx: &egui::Context, env: &GroupEnv) {
        match &self.atlases {
            AtlasState::Unloaded => {
                let (tx, rx) = mpsc::channel();
                let dir = env.atlases_dir.clone();
                let ctx = ctx.clone();
                thread::spawn(move || {
                    let sets = bundled_and_external_atlas_sets(dir.as_deref(), &mut |_| {});
                    let default_name = default_atlas(&sets).map(|a| a.name.clone());
                    let entries = sets.into_iter().map(|a| (a.id, a.name)).collect();
                    let _ = tx.send(AtlasList {
                        entries,
                        default_name,
                    });
                    ctx.request_repaint();
                });
                self.atlases = AtlasState::Loading(rx);
            }
            AtlasState::Loading(rx) => match rx.try_recv() {
                Ok(list) => self.atlases = AtlasState::Ready(list),
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.atlases = AtlasState::Ready(AtlasList::default());
                }
            },
            AtlasState::Ready(_) => {}
        }
    }

    fn atlas_list(&self) -> Option<&AtlasList> {
        match &self.atlases {
            AtlasState::Ready(l) => Some(l),
            _ => None,
        }
    }

    fn status_of(&self, subject: &str) -> Option<&PatientStatus> {
        self.scan.as_ref().and_then(|r| r.statuses.get(subject))
    }

    fn runnable(&self) -> usize {
        self.record
            .patients
            .iter()
            .filter(|p| {
                p.included
                    && self
                        .status_of(&p.subject)
                        .is_some_and(PatientStatus::runnable)
            })
            .count()
    }

    fn refresh_reco(&mut self) {
        self.has_reco = reco_exists(&self.record);
    }

    fn save(&mut self, out: &mut GroupOutput) -> bool {
        match save_group(&self.json_path, &mut self.record) {
            Ok(notes) => {
                for n in notes {
                    out.log.push(format!("dbsloc: warning: group: {n}"));
                }
                self.dirty = false;
                out.log
                    .push(format!("group: saved {}", self.json_path.display()));
                true
            }
            Err(e) => {
                out.errors
                    .push(("Group".to_owned(), format!("not saved: {e}")));
                false
            }
        }
    }
}

fn open_blocking(path: &Path) -> Result<Box<Opened>, String> {
    open_group(path).map(Box::new).map_err(|e| e.to_string())
}

/// Creates the group record (and its Lead Group file) of `dataset_root` with `guid`.
pub fn create_blocking(
    dataset_root: &Path,
    guid: &str,
    title: &str,
) -> Result<Box<Opened>, String> {
    let location = locate(dataset_root, guid.trim(), None, None).map_err(|e| e.to_string())?;
    let (record, json_path, notes) =
        create_group(&location, title.trim(), None).map_err(|e| e.to_string())?;
    Ok(Box::new(Opened {
        record,
        json_path,
        folder: location.folder,
        notes,
    }))
}

impl GroupTab {
    pub fn is_open(&self) -> bool {
        matches!(self.phase, Phase::Open(_))
    }

    pub fn is_opening(&self) -> bool {
        matches!(self.phase, Phase::Opening(_))
    }

    /// `true` when nothing is loading or running.
    pub fn is_settled(&self) -> bool {
        match &self.phase {
            Phase::Idle => true,
            Phase::Opening(_) => false,
            Phase::Open(s) => s.job.is_none(),
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(&self.phase, Phase::Open(s) if s.job.is_some())
    }

    pub fn is_calculating(&self) -> bool {
        self.calc_workers.load(Ordering::SeqCst) > 0
    }

    pub fn rescan(&mut self) {
        if let Phase::Open(s) = &mut self.phase {
            s.rescan = true;
        }
    }

    /// Opens a group record file or folder on a worker thread.
    pub fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.is_busy() || self.is_opening() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.phase = Phase::Opening(rx);
        self.notice = None;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(open_blocking(&path));
            ctx.request_repaint();
        });
    }

    /// Creates a group at `dataset_root` and opens it.
    pub fn create(
        &mut self,
        dataset_root: PathBuf,
        guid: String,
        title: String,
        ctx: &egui::Context,
    ) {
        if self.is_busy() || self.is_opening() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.phase = Phase::Opening(rx);
        self.notice = None;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(create_blocking(&dataset_root, &guid, &title));
            ctx.request_repaint();
        });
    }

    /// Closes the open group; unsaved changes are lost.
    pub fn close(&mut self) {
        if let Phase::Open(s) = &self.phase {
            if let Some(job) = &s.job {
                job.cancel.store(true, Ordering::SeqCst);
            }
        }
        self.phase = Phase::Idle;
        self.popped_out = false;
        self.looked = None;
    }

    /// Writes the 3D view manifest of the open group on a worker thread.
    pub fn start_view(&mut self, env: &GroupEnv, ctx: &egui::Context) {
        let Phase::Open(s) = &mut self.phase else {
            return;
        };
        if s.job.is_some() {
            return;
        }
        let options = view_options(&s.record, env, s.plain_flip);
        let record = s.record.clone();
        let folder = s.folder.clone();
        let (tx, rx) = mpsc::channel();
        s.job = Some(Job {
            kind: JobKind::View,
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            started: Instant::now(),
            done: 0,
            total: 0,
            subject: String::new(),
        });
        s.status = None;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let res = write_group_view(&record, &folder, &options)
                .map(Box::new)
                .map_err(|e| e.to_string());
            let _ = tx.send(JobMsg::View(res));
            ctx.request_repaint();
        });
    }

    /// Saves the group and runs *Calculate* on a worker thread with `run` as the options template.
    pub fn start_calc(&mut self, run: RunOptions, ctx: &egui::Context) -> GroupOutput {
        let mut out = GroupOutput::default();
        let Phase::Open(s) = &mut self.phase else {
            return out;
        };
        if s.job.is_some() {
            return out;
        }
        if !s.save(&mut out) {
            return out;
        }
        let options = calc_options(run, &s.record, s.plain_flip, &s.copy_from);
        let path = s.json_path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        s.job = Some(Job {
            kind: JobKind::Calculate,
            cancel: Arc::clone(&cancel),
            rx,
            started: Instant::now(),
            done: 0,
            total: s.record.patients.iter().filter(|p| p.included).count(),
            subject: String::new(),
        });
        s.status = None;
        out.log.push(format!(
            "group: calculating {} for {} patients",
            s.record.stim_label(),
            s.record.patients.iter().filter(|p| p.included).count()
        ));
        let ctx = ctx.clone();
        spawn_counted(&self.calc_workers, Some(ctx.clone()), move || {
            let sender = tx.clone();
            let repaint = ctx.clone();
            let res = run_calculate_job(&path, &options, Some(&cancel), &mut |ev| {
                let msg = match ev {
                    CalcEvent::SubjectStarted {
                        index,
                        total,
                        subject,
                    } => {
                        let _ =
                            sender.send(JobMsg::Log(format!("[{}/{total}] {subject}", index + 1)));
                        JobMsg::Progress {
                            done: *index,
                            total: *total,
                            subject: subject.clone(),
                        }
                    }
                    CalcEvent::Log { line, .. } => JobMsg::Log(line.clone()),
                    CalcEvent::SubjectFinished(o) => JobMsg::Log(format!(
                        "{}  {}{}",
                        o.subject,
                        o.phase.as_str(),
                        o.message
                            .as_deref()
                            .map_or_else(String::new, |m| format!(": {m}"))
                    )),
                };
                let _ = sender.send(msg);
                repaint.request_repaint();
            });
            let _ = tx.send(JobMsg::Calc(res.map(Box::new).map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
        out
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let Phase::Open(s) = &mut self.phase else {
            return;
        };
        if s.job.is_some() {
            return;
        }
        let record = s.record.clone();
        let folder = s.folder.clone();
        let (tx, rx) = mpsc::channel();
        s.job = Some(Job {
            kind: JobKind::Export,
            cancel: Arc::new(AtomicBool::new(false)),
            rx,
            started: Instant::now(),
            done: 0,
            total: 0,
            subject: String::new(),
        });
        s.status = None;
        let ctx = ctx.clone();
        thread::spawn(move || {
            let mut files: Vec<PathBuf> = Vec::new();
            let mut result: Result<(), String> = Ok(());
            match write_stats_exports(
                &record,
                &folder,
                &ExportOptions {
                    csv: true,
                    lead_dbs_mat: false,
                    out_dir: None,
                },
            ) {
                Ok(done) => files.extend(done.files),
                Err(e) => result = Err(e.to_string()),
            }
            if result.is_ok() && !record.clinical.is_empty() {
                match write_clinical_export(&record, &folder) {
                    Ok((tsv, sidecar)) => files.extend([tsv, sidecar]),
                    Err(e) => result = Err(e.to_string()),
                }
            }
            let _ = tx.send(JobMsg::Export(result.map(|()| files)));
            ctx.request_repaint();
        });
    }

    /// Polls file dialogs, the opening of a group and the running job.
    fn poll(&mut self, ctx: &egui::Context) -> GroupOutput {
        let mut out = GroupOutput::default();
        if let Phase::Opening(rx) = &self.phase {
            match rx.try_recv() {
                Ok(Ok(opened)) => {
                    let path = opened.json_path.to_string_lossy().into_owned();
                    for n in &opened.notes {
                        out.log.push(format!("dbsloc: warning: group: {n}"));
                    }
                    out.log.push(format!("group: opened {path}"));
                    out.remember = Some(path);
                    out.focus = true;
                    self.phase = Phase::Open(Box::new(Session::new(*opened)));
                    self.looked = None;
                }
                Ok(Err(e)) => {
                    self.notice = Some(e.clone());
                    out.errors.push(("Group".to_owned(), e));
                    self.phase = Phase::Idle;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.phase = Phase::Idle,
            }
        }
        if let Some(path) = take_pick(&mut self.picks.open) {
            self.open(path, ctx);
        }
        if let Some(path) = take_pick(&mut self.picks.dataset) {
            self.form.dataset_root = path.to_string_lossy().into_owned();
        }
        if let Phase::Open(s) = &mut self.phase {
            if let Some(path) = take_pick(&mut self.picks.table) {
                import_clinical(s, &path, &mut out);
            }
            s.explorers.cache = self.cache;
            poll_job(s, ctx, &mut out);
            poll_consent(s, ctx, &mut out);
            s.drive_scan(ctx);
        }
        out
    }

    pub fn set_cache(&mut self, cache: dbsloc_pipeline::connectome_cache::CacheSetting) {
        self.cache = cache;
    }

    pub fn pop_out(&mut self) {
        self.popped_out = self.is_open();
    }

    /// Shows the tab docked.
    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, env: &GroupEnv) -> GroupOutput {
        let mut out = self.poll(ctx);
        if self.popped_out && self.is_open() {
            self.popped_notice(ui);
            return out;
        }
        self.body(ui, ctx, env, None, &mut out);
        out
    }

    /// Shows the separate window when the tab is popped out.
    pub fn show_popout(&mut self, ctx: &egui::Context, env: &GroupEnv) -> GroupOutput {
        let mut out = GroupOutput::default();
        if !self.popped_out {
            return out;
        }
        if !self.is_open() {
            self.popped_out = false;
            ctx.request_repaint();
            return out;
        }
        let bring_back = popout_window(
            ctx,
            POPOUT_SALT,
            POPOUT_TITLE,
            POPOUT_NAME,
            true,
            |ui, bar| {
                self.body(ui, ctx, env, Some(bar), &mut out);
            },
        );
        if bring_back {
            self.popped_out = false;
            out.focus = true;
            ctx.request_repaint();
        }
        out
    }

    fn popped_notice(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("The group panel is open in a separate window.")
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            ui.add_space(unit * 0.5);
            if ui
                .button("Dock to main window")
                .on_hover_text("Close the separate window and show the group panel here again")
                .clicked()
            {
                self.popped_out = false;
                ui.ctx().request_repaint();
            }
        });
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        env: &GroupEnv,
        mut bar: Option<&mut PopoutBar>,
        out: &mut GroupOutput,
    ) {
        if !self.is_open() {
            self.idle_ui(ui, ctx, env, out);
            return;
        }
        let mut pop = false;
        let mut close = false;
        let mut open_other = false;
        let mut show_scene = false;
        let mut export = false;
        let mut calc = false;
        let Phase::Open(s) = &mut self.phase else {
            return;
        };
        let pal = palette(ui);
        let unit = em(ui);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "Group · {} · {}",
                        s.record.dataset_name, s.record.guid
                    ))
                    .strong(),
                )
                .selectable(false),
            );
            let busy = s.job.is_some();
            if ui
                .add_enabled(s.dirty && !busy, egui::Button::new("Save"))
                .on_hover_text("Write the group file and its Lead Group file")
                .clicked()
            {
                s.save(out);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Open another group…"))
                .on_hover_text("Close this group and open another group file")
                .clicked()
            {
                open_other = true;
            }
            if let Some(bar) = bar.as_deref_mut() {
                bar.show(ui);
            }
            if bar.is_none()
                && ui
                    .button("Separate window")
                    .on_hover_text("Open the group panel in a separate window")
                    .clicked()
            {
                pop = true;
            }
            if ui
                .button("Close")
                .on_hover_text("Close the group; unsaved changes are lost")
                .clicked()
            {
                close = true;
            }
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(s.json_path.to_string_lossy())
                    .small()
                    .color(pal.text_muted),
            )
            .truncate()
            .selectable(false),
        )
        .on_hover_text("The group record file");
        ui.separator();
        egui::Panel::bottom("group_footer")
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(unit * 0.2);
                footer_ui(
                    s,
                    ui,
                    env,
                    &pal,
                    out,
                    &mut calc,
                    &mut show_scene,
                    &mut export,
                );
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("group_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        s.drive_atlases(ctx, env);
                        sections_ui(s, ui, ctx, env, &pal, out, &mut self.picks);
                    });
            });
        let look = s.look();
        if self.looked.as_ref() != Some(&look) {
            if self.looked.is_some() {
                out.look = Some(look.clone());
            }
            self.looked = Some(look);
        }
        if calc {
            out.calc = true;
        }
        if show_scene {
            self.start_view(env, ctx);
        }
        if export {
            self.start_export(ctx);
        }
        if open_other {
            spawn_pick(ctx, &mut self.picks.open, || {
                rfd::FileDialog::new()
                    .set_title("Open a group file")
                    .add_filter("Group record", &["json"])
                    .pick_file()
            });
        }
        if pop {
            self.popped_out = true;
            ctx.request_repaint();
        }
        if close {
            self.close();
        }
    }

    fn idle_ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        env: &GroupEnv,
        out: &mut GroupOutput,
    ) {
        let pal = palette(ui);
        let unit = em(ui);
        let mut create: Option<(PathBuf, String, String)> = None;
        let mut open: Option<PathBuf> = None;
        egui::ScrollArea::vertical()
            .id_salt("group_idle_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(unit * 0.6);
                if self.is_opening() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(unit * 1.2));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new("Opening the group…").color(pal.text_muted),
                            )
                            .selectable(false),
                        );
                    });
                    return;
                }
                ui.add(
                    egui::Label::new(egui::RichText::new(GROUP_PLACEHOLDER).color(pal.text_muted))
                        .wrap()
                        .selectable(false),
                );
                if let Some(n) = &self.notice {
                    ui.add(
                        egui::Label::new(egui::RichText::new(n).small().color(pal.err)).wrap(),
                    );
                }
                ui.add_space(unit * 0.4);
                if ui
                    .button("Open group…")
                    .on_hover_text("Choose a group record file (.json) written by this software")
                    .clicked()
                {
                    spawn_pick(ctx, &mut self.picks.open, || {
                        rfd::FileDialog::new()
                            .set_title("Open a group file")
                            .add_filter("Group record", &["json"])
                            .pick_file()
                    });
                }
                if !env.recent.is_empty() {
                    ui.add_space(unit * 0.4);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("Recent groups").small().color(pal.text_muted),
                        )
                        .selectable(false),
                    );
                    for r in &env.recent {
                        let name = Path::new(r)
                            .file_name()
                            .map_or_else(|| r.clone(), |n| n.to_string_lossy().into_owned());
                        if ui
                            .button(name)
                            .on_hover_text(format!("Open {r}"))
                            .clicked()
                        {
                            open = Some(PathBuf::from(r));
                        }
                    }
                }
                ui.add_space(unit * 0.8);
                egui::CollapsingHeader::new(egui::RichText::new("New group").strong())
                    .id_salt("group_new")
                    .default_open(true)
                    .show(ui, |ui| {
                        if self.form.dataset_root.is_empty() {
                            if let Some(root) = &env.dataset_root {
                                self.form.dataset_root = root.to_string_lossy().into_owned();
                            }
                        }
                        ui.horizontal(|ui| {
                            let w = (ui.available_width() - unit * 9.0).max(unit * 8.0);
                            labelled_edit(
                                ui,
                                "Dataset folder",
                                &mut self.form.dataset_root,
                                "dataset folder",
                                "The dataset root; the group is stored under derivatives/leadgroup/<id>",
                                w,
                            );
                            if ui
                                .button("Browse…")
                                .on_hover_text("Choose the dataset folder")
                                .clicked()
                            {
                                spawn_pick(ctx, &mut self.picks.dataset, || {
                                    rfd::FileDialog::new()
                                        .set_title("Choose the dataset folder")
                                        .pick_folder()
                                });
                            }
                        });
                        labelled_edit(
                            ui,
                            "Title",
                            &mut self.form.title,
                            "optional",
                            "Free text stored in the group file",
                            unit * 20.0,
                        );
                        labelled_edit(
                            ui,
                            "Identifier",
                            &mut self.form.guid,
                            "letters and digits",
                            "Names the group folder and file; letters and digits only",
                            unit * 12.0,
                        );
                        let ready = !self.form.dataset_root.trim().is_empty()
                            && !self.form.guid.trim().is_empty();
                        if ui
                            .add_enabled(ready, egui::Button::new("Create group"))
                            .on_hover_text("Create the group file and its Lead Group file")
                            .clicked()
                        {
                            create = Some((
                                PathBuf::from(self.form.dataset_root.trim()),
                                self.form.guid.trim().to_owned(),
                                self.form.title.clone(),
                            ));
                        }
                    });
            });
        if let Some(path) = open {
            self.open(path, ctx);
        }
        if let Some((root, guid, title)) = create {
            self.create(root, guid, title, ctx);
        }
        let _ = out;
    }
}

fn labelled_edit(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    tooltip: &str,
    width: f32,
) -> egui::Response {
    ui.horizontal(|ui| {
        let l = ui.label(label);
        ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width(width),
        )
        .labelled_by(l.id)
        .on_hover_text(tooltip)
    })
    .inner
}

fn import_clinical(s: &mut Session, path: &Path, out: &mut GroupOutput) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            out.errors.push((
                "Group".to_owned(),
                format!("cannot read {}: {e}", path.display()),
            ));
            return;
        }
    };
    let delimiter = match delimiter_for(path) {
        Ok(d) => d,
        Err(e) => {
            out.errors.push(("Group".to_owned(), e.to_string()));
            return;
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let options = ImportOptions {
        replace: s.replace_clinical,
        subject_column: None,
        source: format!("tsv:{name}"),
    };
    match import_table(&mut s.record, &text, delimiter, &options) {
        Ok(report) => {
            s.dirty = true;
            out.log.push(format!(
                "group: imported {} clinical variable(s) from {name}",
                report.imported.len() + report.replaced.len()
            ));
            if !report.unknown_subjects.is_empty() {
                out.log.push(format!(
                    "dbsloc: warning: group: rows of unknown subjects skipped: {}",
                    report.unknown_subjects.join(", ")
                ));
            }
            s.status = Some(format!(
                "Imported {} variable(s) from {name}",
                report.imported.len() + report.replaced.len()
            ));
        }
        Err(e) => out.errors.push(("Group".to_owned(), e.to_string())),
    }
}

fn poll_job(s: &mut Session, ctx: &egui::Context, out: &mut GroupOutput) {
    let Some(mut job) = s.job.take() else {
        return;
    };
    let mut finished = false;
    loop {
        match job.rx.try_recv() {
            Ok(JobMsg::Log(line)) => out.log.push(line),
            Ok(JobMsg::Progress {
                done,
                total,
                subject,
            }) => {
                job.done = done;
                job.total = total;
                job.subject = subject;
            }
            Ok(JobMsg::Calc(res)) => {
                finished = true;
                match res {
                    Ok(report) => {
                        let cancelled = report.cancelled();
                        out.log.push(format!(
                            "group: {} subjects: {} computed, {} kept, {} failed, {} cancelled",
                            report.outcomes.len(),
                            report
                                .outcomes
                                .iter()
                                .filter(|o| o.phase.as_str() == "computed")
                                .count(),
                            report
                                .outcomes
                                .iter()
                                .filter(|o| o.phase.as_str() == "kept")
                                .count(),
                            report
                                .outcomes
                                .iter()
                                .filter(|o| o.phase.is_failure())
                                .count(),
                            report
                                .outcomes
                                .iter()
                                .filter(|o| o.phase.as_str() == "cancelled")
                                .count(),
                        ));
                        for n in &report.notes {
                            out.log.push(format!("dbsloc: warning: group: {n}"));
                        }
                        s.status = Some(if cancelled {
                            "Computation cancelled".to_owned()
                        } else if report.any_failure() {
                            "Computation finished with failures; see the Output panel".to_owned()
                        } else {
                            "Computation finished".to_owned()
                        });
                        if report.any_failure() {
                            out.errors.push((
                                "Group".to_owned(),
                                report
                                    .outcomes
                                    .iter()
                                    .filter(|o| o.phase.is_failure())
                                    .map(|o| {
                                        format!(
                                            "{}: {}",
                                            o.subject,
                                            o.message.as_deref().unwrap_or("failed")
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ));
                        }
                    }
                    Err(e) => out.errors.push(("Group".to_owned(), e)),
                }
                if let Ok(opened) = open_group(&s.json_path) {
                    s.record = opened.record;
                    s.dirty = false;
                }
                s.refresh_reco();
                s.rescan = true;
            }
            Ok(JobMsg::View(res)) => {
                finished = true;
                match res {
                    Ok(report) => {
                        for n in &report.notes {
                            out.log.push(format!("dbsloc: warning: group: {n}"));
                        }
                        for (subject, why) in &report.skipped {
                            out.log.push(format!(
                                "dbsloc: warning: group: {subject} left out of the 3D view: {why}"
                            ));
                        }
                        out.log.push(format!(
                            "group: 3D view of {} patients, {} leads: {}",
                            report.patients,
                            report.leads,
                            report.manifest.display()
                        ));
                        s.status = Some(format!(
                            "3D view: {} patients, {} leads",
                            report.patients, report.leads
                        ));
                        out.show_scene = Some(report.manifest.clone());
                        out.look = Some(s.look());
                    }
                    Err(e) => out.errors.push(("Group 3D view".to_owned(), e)),
                }
            }
            Ok(JobMsg::Export(res)) => {
                finished = true;
                match res {
                    Ok(files) => {
                        for f in &files {
                            out.log.push(format!("group: wrote {}", f.display()));
                        }
                        s.status = Some(format!("Exported {} file(s)", files.len()));
                        s.export_dir = common_parent(&files);
                    }
                    Err(e) => out.errors.push(("Group export".to_owned(), e)),
                }
            }
            Ok(JobMsg::Analysis(res)) => {
                finished = true;
                match res {
                    Ok(done) => {
                        done.kind.register(&mut s.record, &done.id);
                        out.log.push(done.summary.clone());
                        for n in &done.notes {
                            out.log.push(format!("dbsloc: warning: group: {n}"));
                        }
                        s.dirty = true;
                        if s.save(out) {
                            s.status = Some(format!("{} {} finished", done.kind.label(), done.id));
                        }
                    }
                    Err(AnalysisFail::Cancelled) => {
                        out.log.push("group: analysis cancelled".to_owned());
                        s.status = Some("Analysis cancelled".to_owned());
                    }
                    Err(AnalysisFail::Failed(message)) => {
                        out.errors.push(("Group analysis".to_owned(), message));
                    }
                }
            }
            Ok(JobMsg::Layers(res)) => {
                finished = true;
                match res {
                    Ok(layers) => {
                        s.status = Some("Result sent to the 3D viewer".to_owned());
                        out.layers.extend(layers);
                    }
                    Err(e) => out.errors.push(("Group analysis".to_owned(), e)),
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                finished = true;
                break;
            }
        }
    }
    if !finished {
        s.job = Some(job);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn compute_tip(ready: bool, runnable: usize) -> &'static str {
    if !ready {
        "Wait until the running step has finished"
    } else if runnable == 0 {
        "No patient is ready: a patient needs a reconstruction and stimulation settings"
    } else {
        "Save the group, then for every included patient with a reconstruction and stimulation settings compute the volume of tissue activated, the atlas overlap statistics and the contact distances (plus the mirrored volumes when Mirror sides is on); unchanged results are kept"
    }
}

#[allow(clippy::too_many_arguments)]
fn footer_ui(
    s: &mut Session,
    ui: &mut egui::Ui,
    env: &GroupEnv,
    pal: &crate::app::tokens::Palette,
    out: &mut GroupOutput,
    calc: &mut bool,
    show_scene: &mut bool,
    export: &mut bool,
) {
    let busy = s.job.is_some();
    if let Some(job) = &s.job {
        ui.horizontal(|ui| {
            if job.total > 0 {
                ui.add(
                    egui::ProgressBar::new(job.done as f32 / job.total as f32)
                        .desired_width(em(ui) * 16.0)
                        .text(format!(
                            "{} {} / {}{}",
                            job.kind.label(),
                            job.done,
                            job.total,
                            if job.subject.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", job.subject)
                            }
                        )),
                );
            } else {
                ui.add(egui::Spinner::new().size(em(ui)));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(if job.subject.is_empty() {
                            job.kind.label().to_owned()
                        } else {
                            format!("{} · {}", job.kind.label(), job.subject)
                        })
                        .color(pal.text_muted),
                    )
                    .selectable(false),
                );
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{} s", job.started.elapsed().as_secs()))
                        .small()
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            if job.kind == JobKind::Calculate
                && ui
                    .button("Cancel")
                    .on_hover_text("Stop after the step that is running; the remaining patients are not computed")
                    .clicked()
            {
                job.cancel.store(true, Ordering::SeqCst);
            }
            if job.kind == JobKind::Analysis
                && ui
                    .button("Cancel")
                    .on_hover_text("Stop the analysis; nothing is saved")
                    .clicked()
            {
                job.cancel.store(true, Ordering::SeqCst);
            }
        });
    }
    let runnable = s.runnable();
    let atlas = atlas_summary_name(s.atlas_list(), s.record.atlas_set.as_deref());
    let source = s.scan_key().source;
    ui.add(
        egui::Label::new(
            egui::RichText::new(compute_summary(
                runnable,
                source.as_deref(),
                &atlas,
                s.record.view.mirror_sides,
            ))
            .small()
            .color(pal.text_muted),
        )
        .wrap(),
    );
    ui.horizontal_wrapped(|ui| {
        let ready = !busy && !env.run_active;
        if ui
            .add_enabled(ready && runnable > 0, egui::Button::new("Compute VTAs"))
            .on_hover_text(compute_tip(ready, runnable))
            .clicked()
        {
            *calc = true;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Show in 3D viewer"))
            .on_hover_text("Draw every patient's leads, contacts and volumes of tissue activated in the 3D viewer")
            .clicked()
        {
            *show_scene = true;
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Export tables"))
            .on_hover_text(
                "Write the atlas overlap tables (TSV and CSV) and the clinical table under the group's exports folder",
            )
            .clicked()
        {
            *export = true;
        }
        if env.run_active {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("A pipeline run is active").small().color(pal.warn),
                )
                .selectable(false),
            );
        }
    });
    if let Some(status) = &s.status {
        ui.add(egui::Label::new(egui::RichText::new(status).small().color(pal.ok)).wrap());
    }
    if let Some(dir) = s.export_dir.clone() {
        ui.horizontal(|ui| {
            let room = ((ui.available_width() - em(ui) * 8.0) / (em(ui) * 0.5)).max(16.0) as usize;
            ui.add(
                egui::Label::new(
                    egui::RichText::new(crate::panels::shorten_path(&dir, room))
                        .small()
                        .color(pal.text_muted),
                )
                .selectable(false),
            )
            .on_hover_text(dir.display().to_string());
            if crate::panels::open_folder_button(
                ui,
                "Open the folder with the exported tables in the file manager",
            ) {
                if let Err(e) = crate::panels::reveal_in_file_manager(&dir) {
                    out.errors.push(("Group export".to_owned(), e));
                }
            }
        });
    }
}

fn common_parent(files: &[PathBuf]) -> Option<PathBuf> {
    let mut parents = files.iter().filter_map(|f| f.parent());
    let mut common = parents.next()?.to_path_buf();
    for p in parents {
        while !p.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

fn section(
    ui: &mut egui::Ui,
    id: &'static str,
    title: &str,
    tip: &str,
    open: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong())
        .id_salt(id)
        .default_open(open)
        .show(ui, body)
        .header_response
        .on_hover_text(tip);
}

fn sections_ui(
    s: &mut Session,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    env: &GroupEnv,
    pal: &crate::app::tokens::Palette,
    out: &mut GroupOutput,
    picks: &mut Picks,
) {
    let unit = em(ui);
    for n in &s.notes {
        ui.add(egui::Label::new(egui::RichText::new(n.to_string()).small().color(pal.warn)).wrap());
    }
    section(
        ui,
        "group_patients",
        "Patients",
        "The patients of the group and what is still missing for each",
        !s.explorers.open,
        |ui| {
            patients_ui(s, ui, env, pal, out, unit);
        },
    );
    let label = s.record.stim_label();
    section(
        ui,
        "group_stim",
        "Stimulation",
        &format!("The stimulation of the group, stored under the label {label} for every patient"),
        !s.explorers.open,
        |ui| {
            stimulation_ui(s, ui, unit);
        },
    );
    section(
        ui,
        "group_groups",
        "Groups",
        "Group numbers, labels and colours",
        false,
        |ui| {
            groups_ui(s, ui);
        },
    );
    section(
        ui,
        "group_clinical",
        "Clinical variables",
        "Clinical scores per patient, used for colouring and statistics",
        false,
        |ui| {
            clinical_ui(s, ui, ctx, pal, picks);
        },
    );
    let analyses_open = s.explorers.open;
    section(
        ui,
        "group_analyses",
        "Analyses",
        "Sweet-spot mapping, fibre filtering and network mapping of a clinical variable",
        analyses_open,
        |ui| {
            analyses_ui(s, ui, ctx, env, pal, out);
        },
    );
    section(
        ui,
        "group_display",
        "Display",
        "What the 3D viewer draws and whether sides are mirrored",
        false,
        |ui| {
            display_ui(s, ui);
        },
    );
}

fn analyses_ui(
    s: &mut Session,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    env: &GroupEnv,
    pal: &crate::app::tokens::Palette,
    out: &mut GroupOutput,
) {
    let included = s.record.patients.iter().filter(|p| p.included).count();
    let computed = s
        .record
        .patients
        .iter()
        .filter(|p| p.included && matches!(s.status_of(&p.subject), Some(PatientStatus::Computed)))
        .count();
    let cx = explorers::Cx {
        record: &s.record,
        env,
        folder: &s.folder,
        busy: s.job.is_some() || s.explorers.consent.is_active(),
        included,
        computed,
    };
    let requests = explorers::show(&mut s.explorers, ui, &cx, pal);
    if let Some(choice) = s.explorers.remembered.take() {
        out.remember_connectome = Some(choice);
    }
    for request in requests {
        match request {
            Request::Run => start_analysis(s, env, ctx, out),
            Request::Show(kind, id) => start_layers(s, kind, &id, ctx),
            Request::OpenFolder(path) => {
                if let Err(e) = crate::panels::reveal_in_file_manager(&path) {
                    out.errors.push(("Group analysis".to_owned(), e));
                }
            }
            Request::Delete(kind, id) => {
                match explorers::delete_analysis(&s.folder, &mut s.record, kind, &id) {
                    Ok(()) => {
                        out.log.push(format!(
                            "group: deleted {} {id}",
                            kind.label().to_lowercase()
                        ));
                        out.remove_layers.extend(explorers::layer_ids(kind, &id));
                        s.dirty = true;
                        s.save(out);
                    }
                    Err(e) => out.errors.push(("Group analysis".to_owned(), e)),
                }
            }
        }
    }
}

fn start_analysis(s: &mut Session, env: &GroupEnv, ctx: &egui::Context, out: &mut GroupOutput) {
    if s.job.is_some() {
        return;
    }
    let Some(variable) = s.explorers.variable.clone() else {
        return;
    };
    let kind = s.explorers.kind;
    let connectome = s.explorers.connectome.get(&kind).cloned();
    let Some(plan) = explorers::plan(
        &s.explorers,
        &s.folder,
        &variable,
        connectome.as_deref(),
        env,
    ) else {
        return;
    };
    if s.dirty && !s.save(out) {
        return;
    }
    let setting = s.explorers.cache;
    if explorers::plan_asks(&plan, setting) {
        let record = s.record.clone();
        let probe_plan = plan.clone();
        s.explorers.pending = Some(plan);
        s.explorers.consent.begin(ctx, setting, move || {
            explorers::consent_probe(&record, &probe_plan)
        });
        return;
    }
    launch_analysis(s, plan, CacheRun::plain(setting), variable, ctx, out);
}

fn poll_consent(s: &mut Session, ctx: &egui::Context, out: &mut GroupOutput) {
    if let Some(run) = s.explorers.consent.poll(ctx) {
        if let Some(plan) = s.explorers.pending.take() {
            s.explorers.answered = Some((plan, run));
        }
    }
    if s.job.is_some() {
        return;
    }
    let Some((plan, run)) = s.explorers.answered.take() else {
        return;
    };
    let Some(variable) = s.explorers.variable.clone() else {
        out.log
            .push("dbsloc: note: analysis not started: no variable chosen".to_owned());
        return;
    };
    launch_analysis(s, plan, run, variable, ctx, out);
}

fn launch_analysis(
    s: &mut Session,
    plan: explorers::Plan,
    run: CacheRun,
    variable: String,
    ctx: &egui::Context,
    out: &mut GroupOutput,
) {
    let kind = s.explorers.kind;
    let record = s.record.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    s.job = Some(Job {
        kind: JobKind::Analysis,
        cancel: Arc::clone(&cancel),
        rx,
        started: Instant::now(),
        done: 0,
        total: 0,
        subject: String::new(),
    });
    s.status = None;
    out.log.push(format!(
        "group: running {} on {variable}",
        kind.label().to_lowercase()
    ));
    let ctx = ctx.clone();
    thread::spawn(move || {
        let sender = tx.clone();
        let repaint = ctx.clone();
        let sink = move |u: explorers::Update| {
            let _ = sender.send(JobMsg::Progress {
                done: u.done,
                total: u.total,
                subject: u.text,
            });
            repaint.request_repaint();
        };
        let res = explorers::run_plan(&record, &plan, &run, &cancel, &sink);
        let _ = tx.send(JobMsg::Analysis(res.map(Box::new)));
        ctx.request_repaint();
    });
}

fn start_layers(s: &mut Session, kind: Kind, id: &str, ctx: &egui::Context) {
    if s.job.is_some() {
        return;
    }
    let folder = s.folder.clone();
    let id = id.to_owned();
    let (tx, rx) = mpsc::channel();
    s.job = Some(Job {
        kind: JobKind::Layers,
        cancel: Arc::new(AtomicBool::new(false)),
        rx,
        started: Instant::now(),
        done: 0,
        total: 0,
        subject: String::new(),
    });
    s.status = None;
    let ctx = ctx.clone();
    thread::spawn(move || {
        let _ = tx.send(JobMsg::Layers(explorers::load_layers(&folder, kind, &id)));
        ctx.request_repaint();
    });
}

fn patients_ui(
    s: &mut Session,
    ui: &mut egui::Ui,
    env: &GroupEnv,
    pal: &crate::app::tokens::Palette,
    out: &mut GroupOutput,
    unit: f32,
) {
    let choice = add_choice(&env.ticked_subjects, env.selected_subject.as_deref());
    let no_dataset = env.dataset_root.is_none();
    let add_tip = if choice.ids.is_empty() {
        "Tick subjects in the Subjects panel"
    } else if no_dataset {
        "Open a dataset folder first"
    } else if choice.ticked {
        "Add every subject ticked in the Subjects panel to the group; subjects already in it are skipped"
    } else {
        "Add the subject highlighted in the Subjects panel to the group"
    };
    if ui
        .add_enabled(
            !choice.ids.is_empty() && !no_dataset,
            egui::Button::new(choice.label.as_str()),
        )
        .on_hover_text(add_tip)
        .clicked()
    {
        add_chosen(s, env, &choice.ids, out);
    }
    ui.horizontal_wrapped(|ui| {
        labelled_edit(
            ui,
            "Add patients",
            &mut s.add_ids,
            "subject ids, e.g. 01 02 03",
            "Subject ids of the current dataset, separated by spaces or commas; they join group 1",
            unit * 14.0,
        );
        let ready = env.dataset_root.is_some() && !s.add_ids.trim().is_empty();
        if ui
            .add_enabled(ready, egui::Button::new("Add"))
            .on_hover_text("Add the listed subjects of the current dataset to the group")
            .clicked()
        {
            let ids = parse_ids(&s.add_ids);
            add_chosen(s, env, &ids, out);
            s.add_ids.clear();
        }
    });
    if s.record.patients.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new("No patients yet")
                    .small()
                    .color(pal.text_muted),
            )
            .selectable(false),
        );
        return;
    }
    let source = s.scan_key().source;
    let line = status_line(
        s.record.patients.len(),
        s.record
            .patients
            .iter()
            .filter_map(|p| s.status_of(&p.subject)),
    );
    ui.add(
        egui::Label::new(egui::RichText::new(line).small().color(pal.text_muted)).selectable(false),
    );
    let w_check = unit * 1.6;
    let w_subject = unit * 9.0;
    let w_group = unit * 5.0;
    let w_status = unit * 11.0;
    ui.horizontal(|ui| {
        for (w, text, tip) in [
            (
                w_check,
                "Use",
                "Include the patient in the view and in Compute VTAs",
            ),
            (
                w_subject,
                "Subject",
                "Select a patient to highlight its leads in the 3D viewer",
            ),
            (w_group, "Group", "Group number of the patient"),
            (
                w_status,
                "Status",
                "Whether the patient can be computed: it needs a reconstruction and stimulation settings",
            ),
        ] {
            ui.add_sized(
                [w, unit],
                egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted))
                    .selectable(false),
            )
            .on_hover_text(tip);
        }
    });
    let row_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
    let n = s.record.patients.len();
    let max_h = (row_h * 10.0).max(unit * 8.0);
    let mut remove: Option<usize> = None;
    let mut select: Option<usize> = None;
    let mut edit: Option<usize> = None;
    egui::ScrollArea::vertical()
        .id_salt("group_patient_rows")
        .max_height(max_h)
        .auto_shrink([false, true])
        .show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let status = s.status_of(&s.record.patients[i].subject).cloned();
                let selected = s.selected == Some(i);
                let edit_reason = {
                    let p = &s.record.patients[i];
                    let has_reco = s.has_reco.get(i).copied().unwrap_or(false);
                    let in_dataset = env
                        .derivatives_root
                        .as_ref()
                        .zip(s.record.patient_folder(p))
                        .is_some_and(|(root, folder)| same_folder(&folder, &root.join(&p.subject)));
                    edit_block_reason(has_reco, in_dataset, s.job.is_some() || env.run_active)
                };
                let p = &mut s.record.patients[i];
                ui.horizontal(|ui| {
                    let mut included = p.included;
                    if ui
                        .add_sized([w_check, unit], egui::Checkbox::without_text(&mut included))
                        .on_hover_text("Include this patient in the 3D view and in Compute VTAs")
                        .changed()
                    {
                        p.included = included;
                        s.dirty = true;
                    }
                    if ui
                        .add_sized(
                            [w_subject, unit],
                            egui::Button::selectable(selected, p.subject.clone()),
                        )
                        .on_hover_text("Highlight this patient in the 3D viewer")
                        .clicked()
                    {
                        select = Some(i);
                    }
                    let mut group = p.group;
                    if ui
                        .add_sized(
                            [w_group, unit],
                            egui::DragValue::new(&mut group).range(1..=99),
                        )
                        .on_hover_text("Group number; groups are listed below")
                        .changed()
                    {
                        p.group = group;
                        s.dirty = true;
                    }
                    let (text, colour, tip) = match &status {
                        Some(st) => (st.text(), st.colour(pal), st.tip(source.as_deref())),
                        None => (
                            "Checking…",
                            pal.text_muted,
                            "Looking at the patient's files".to_owned(),
                        ),
                    };
                    ui.add_sized(
                        [w_status, unit],
                        egui::Label::new(egui::RichText::new(text).small().color(colour))
                            .selectable(false),
                    )
                    .on_hover_text(tip);
                    if ui
                        .add_enabled(edit_reason.is_none(), egui::Button::new("Edit").small())
                        .on_hover_text(
                            edit_reason
                                .unwrap_or("Edit the stimulation this patient uses in the group"),
                        )
                        .clicked()
                    {
                        edit = Some(i);
                    }
                    if ui
                        .small_button("Remove")
                        .on_hover_text("Remove this patient from the group")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            }
        });
    if s.record
        .patients
        .iter()
        .any(|p| !s.record.groups.iter().any(|g| g.number == p.group))
    {
        s.record.ensure_groups();
    }
    if let Some(i) = select {
        s.selected = if s.selected == Some(i) { None } else { Some(i) };
    }
    if let Some(i) = edit {
        out.edit_stimulation = Some(edit_focus(
            &s.record.patients[i].subject,
            (!s.copy_from.trim().is_empty()).then_some(s.copy_from.as_str()),
            &s.record.stim_label(),
        ));
    }
    if let Some(i) = remove {
        let name = s.record.patients[i].subject.clone();
        s.record.remove_patient(&name);
        s.has_reco.remove(i.min(s.has_reco.len().saturating_sub(1)));
        s.selected = None;
        s.dirty = true;
        out.log.push(format!("group: removed {name}"));
        s.refresh_reco();
    }
}

fn add_patients(s: &mut Session, env: &GroupEnv, ids: &[String], out: &mut GroupOutput) -> usize {
    let Some(root) = env.dataset_root.as_ref() else {
        return 0;
    };
    let derivatives = env
        .derivatives_root
        .clone()
        .unwrap_or_else(|| root.join("derivatives").join("leaddbs"));
    let dataset = s
        .record
        .ensure_dataset(&root.to_string_lossy(), &derivatives.to_string_lossy());
    let mut added = 0;
    for id in ids {
        match s.record.add_patient(id, &dataset, 1) {
            Ok(()) => added += 1,
            Err(e) => out.errors.push(("Group".to_owned(), e.to_string())),
        }
    }
    if added > 0 {
        s.dirty = true;
        s.refresh_reco();
        out.log.push(format!("group: added {added} patient(s)"));
    }
    added
}

fn add_chosen(s: &mut Session, env: &GroupEnv, ids: &[String], out: &mut GroupOutput) {
    let (present, new): (Vec<&String>, Vec<&String>) = ids
        .iter()
        .partition(|id| s.record.patient_index(id).is_some());
    let new: Vec<String> = new.into_iter().cloned().collect();
    let added = add_patients(s, env, &new, out);
    if !present.is_empty() {
        out.log.push(format!(
            "group: skipped {} already in the group: {}",
            present.len(),
            present
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    s.status = Some(if present.is_empty() {
        format!("Added {}", plural(added, "patient", "patients"))
    } else {
        format!(
            "Added {} · {} already in the group",
            plural(added, "patient", "patients"),
            present.len()
        )
    });
}

fn groups_ui(s: &mut Session, ui: &mut egui::Ui) {
    let unit = em(ui);
    let mut changed = false;
    let mut select = None;
    for g in &mut s.record.groups {
        ui.horizontal(|ui| {
            let selected = s.selected_group == Some(g.number);
            if ui
                .add_sized(
                    [unit * 4.0, unit],
                    egui::Button::selectable(selected, format!("Group {}", g.number)),
                )
                .on_hover_text("Highlight the patients of this group in the 3D viewer")
                .clicked()
            {
                select = Some(g.number);
            }
            let l = ui.label("Label");
            if ui
                .add(egui::TextEdit::singleline(&mut g.label).desired_width(unit * 10.0))
                .labelled_by(l.id)
                .on_hover_text("Name of the group, shown in the legend")
                .changed()
            {
                changed = true;
            }
            let rgb64 = g.color.unwrap_or_else(|| default_color(g.number));
            let mut rgb = [rgb64[0] as f32, rgb64[1] as f32, rgb64[2] as f32];
            if ui
                .color_edit_button_rgb(&mut rgb)
                .on_hover_text("Colour of the group's leads in the 3D viewer")
                .changed()
            {
                g.color = Some([f64::from(rgb[0]), f64::from(rgb[1]), f64::from(rgb[2])]);
                changed = true;
            }
        });
    }
    if let Some(n) = select {
        s.selected_group = if s.selected_group == Some(n) {
            None
        } else {
            Some(n)
        };
    }
    if changed {
        s.dirty = true;
    }
}

fn clinical_ui(
    s: &mut Session,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    pal: &crate::app::tokens::Palette,
    picks: &mut Picks,
) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Import table…")
            .on_hover_text(
                "Import clinical variables from a TSV or CSV table with a subject column; every other column becomes a variable",
            )
            .clicked()
        {
            spawn_pick(ctx, &mut picks.table, || {
                rfd::FileDialog::new()
                    .set_title("Import clinical variables")
                    .add_filter("Table", &["tsv", "csv"])
                    .pick_file()
            });
        }
        ui.checkbox(&mut s.replace_clinical, "Replace existing")
            .on_hover_text("Replace variables that already exist under the same name");
    });
    if s.record.clinical.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new("No clinical variables")
                    .small()
                    .color(pal.text_muted),
            )
            .selectable(false),
        );
        return;
    }
    let mut remove: Option<usize> = None;
    for (i, v) in s.record.clinical.iter().enumerate() {
        ui.horizontal(|ui| {
            let present = v
                .values
                .iter()
                .filter(|r| r.iter().any(Option::is_some))
                .count();
            ui.add(
                egui::Label::new(format!(
                    "{} · {} of {} patients",
                    v.name,
                    present,
                    v.values.len()
                ))
                .selectable(false),
            );
            if ui
                .small_button("Remove")
                .on_hover_text("Remove this clinical variable")
                .clicked()
            {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        s.record.clinical.remove(i);
        s.record
            .view
            .colour_by
            .retain(|n| s.record.clinical.iter().any(|v| &v.name == n));
        s.dirty = true;
    }
    let current = s
        .record
        .view
        .colour_by
        .first()
        .filter(|_| s.record.view.colour_contacts)
        .cloned();
    let mut choice = current.clone();
    egui::ComboBox::from_label("Colour by")
        .selected_text(choice.clone().unwrap_or_else(|| "Group".to_owned()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut choice, None, "Group");
            for v in &s.record.clinical {
                ui.selectable_value(&mut choice, Some(v.name.clone()), &v.name);
            }
        })
        .response
        .on_hover_text("Colour the contacts by group, or by a clinical variable (blue to red)");
    if choice != current {
        s.record.view.colour_contacts = choice.is_some();
        s.record.view.colour_by = choice.into_iter().collect();
        s.dirty = true;
    }
    let mut norm = normalisation_of(s.record.view.normalisation);
    egui::ComboBox::from_label("Normalisation")
        .selected_text(normalisation_label(norm))
        .show_ui(ui, |ui| {
            for n in [
                Normalisation::None,
                Normalisation::ZScore,
                Normalisation::VanAlbada,
            ] {
                ui.selectable_value(&mut norm, n, normalisation_label(n));
            }
        })
        .response
        .on_hover_text("Normalise the values per side before colouring");
    if normalisation_code(norm) != s.record.view.normalisation {
        s.record.view.normalisation = normalisation_code(norm);
        s.dirty = true;
    }
}

fn stimulation_ui(s: &mut Session, ui: &mut egui::Ui, _unit: f32) {
    egui::Grid::new("group_stimulation_grid")
        .num_columns(2)
        .show(ui, |ui| stimulation_rows(s, ui));
}

fn stimulation_rows(s: &mut Session, ui: &mut egui::Ui) {
    let labels = s
        .scan
        .as_ref()
        .map(|r| r.labels.clone())
        .unwrap_or_default();
    let total = s.record.patients.len();
    let current = (!s.copy_from.trim().is_empty()).then(|| s.copy_from.trim().to_owned());
    let mut choice = current.clone();
    ui.label("Settings from");
    egui::ComboBox::from_id_salt("group_settings_from")
        .selected_text(match &choice {
            Some(l) => source_label(l, &labels, total),
            None => "Group settings only".to_owned(),
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut choice, None, "Group settings only");
            let mut shown = labels.clone();
            if let Some(c) = &current {
                if !shown.iter().any(|(l, _)| l == c) {
                    shown.push((c.clone(), 0));
                }
            }
            for (l, _) in &shown {
                ui.selectable_value(&mut choice, Some(l.clone()), source_label(l, &labels, total));
            }
        })
        .response
        .on_hover_text(format!(
            "Each patient's stimulation with this label is copied into the group ({}) and kept in step with it; patients without it keep their earlier group settings",
            s.record.stim_label()
        ));
    ui.end_row();
    if choice != current {
        s.copy_from = choice.unwrap_or_default();
    }
    let current = s.record.stimulation.as_ref().and_then(|x| x.model);
    let mut model = current;
    ui.label("Model");
    egui::ComboBox::from_id_salt("group_model")
        .selected_text(model.map_or("From the settings", ModelKind::display_name))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut model, None, "From the settings");
            for m in ModelKind::ALL {
                ui.selectable_value(&mut model, Some(m), m.display_name());
            }
        })
        .response
        .on_hover_text(
            "One model for the whole group; patients whose settings use another model fail",
        );
    ui.end_row();
    if model != current {
        s.record.stimulation = Some(StimulationRef {
            label: s.record.stim_label(),
            model,
            settings_ref: "per_subject".to_owned(),
        });
        s.dirty = true;
    }
    let mut atlas = s.record.atlas_set.clone();
    let list = s.atlas_list();
    ui.label("Atlas set");
    egui::ComboBox::from_id_salt("group_atlas_set")
        .selected_text(atlas_text(list, atlas.as_deref()))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut atlas, None, atlas_text(list, None));
            let mut extra = atlas.clone();
            if let Some(l) = list {
                for (id, name) in &l.entries {
                    ui.selectable_value(&mut atlas, Some(id.clone()), name);
                    if extra.as_deref() == Some(id.as_str()) {
                        extra = None;
                    }
                }
            }
            if let Some(id) = extra {
                ui.selectable_value(&mut atlas, Some(id.clone()), id);
            }
        })
        .response
        .on_hover_text("The atlas set of the overlap statistics and of the 3D view");
    ui.end_row();
    if atlas != s.record.atlas_set {
        s.record.atlas_set = atlas;
        s.dirty = true;
    }
}

fn display_ui(s: &mut Session, ui: &mut egui::Ui) {
    let v = &mut s.record.view;
    let before = (
        v.show_active,
        v.show_passive,
        v.highlight_active,
        v.mirror_sides,
    );
    ui.checkbox(&mut v.show_active, "Show active contacts")
        .on_hover_text("Draw the contacts that carry current");
    ui.checkbox(&mut v.show_passive, "Show passive contacts")
        .on_hover_text("Draw the contacts that carry no current");
    ui.checkbox(&mut v.highlight_active, "Highlight active contacts")
        .on_hover_text(
            "Draw active contacts in red at full size and passive contacts at half size",
        );
    ui.checkbox(&mut v.mirror_sides, "Mirror sides")
        .on_hover_text(
            "Add a mirrored copy of every patient (sides swapped and flipped) to the 3D view; Compute VTAs then writes the flipped volumes",
        );
    ui.checkbox(&mut s.plain_flip, "Plain x flip for mirroring")
        .on_hover_text(
            "Mirror with a plain left-right flip instead of the non-linear flip transform of the template space",
        );
    let v = &s.record.view;
    if before
        != (
            v.show_active,
            v.show_passive,
            v.highlight_active,
            v.mirror_sides,
        )
    {
        s.dirty = true;
    }
}

#[cfg(test)]
#[path = "group_panel_tests.rs"]
mod tests;

#[cfg(test)]
mod calc_worker_tests {
    use super::*;

    #[test]
    fn closing_the_group_keeps_the_calculation_counted_until_its_worker_ends() {
        let mut tab = GroupTab::default();
        assert!(!tab.is_calculating());
        let (release, wait_for_release) = mpsc::channel::<()>();
        let worker = spawn_counted(&tab.calc_workers, None, move || {
            let _ = wait_for_release.recv();
        });
        assert!(tab.is_calculating());
        tab.close();
        assert!(tab.is_calculating());
        drop(release);
        assert!(worker.join().is_ok());
        assert!(!tab.is_calculating());

        let failing = spawn_counted(&tab.calc_workers, None, || panic!("worker failed"));
        assert!(failing.join().is_err());
        assert!(!tab.is_calculating());
    }
}
