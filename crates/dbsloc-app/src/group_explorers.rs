// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;

use dbsloc_connectome::{read_fibres, FunctionalConnectome};
use dbsloc_pipeline::connectome_cache::CacheSetting;
use dbsloc_pipeline::connectome_catalog::{ConnectomeClass, REFERENCE_SPACE};
use dbsloc_pipeline::connectome_fetch::{discover_connectomes, ConnectomeRoots};
use dbsloc_pipeline::connectome_maps::job::{
    find_side_files, resolve_connectome, run_functional_batch, run_structural, stimulation_folder,
    FunctionalPass, FunctionalRequest, LoadOptions, ResolvedConnectome, Status, StructuralEngine,
    StructuralRequest, SubjectOutcome,
};
use dbsloc_pipeline::connectome_maps::nii::read_nii;
use dbsloc_pipeline::connectome_maps::structural::{OutputGrid, SeedKind};
use dbsloc_pipeline::connectome_maps::{MapError, Progress};
use dbsloc_pipeline::group::fibrefilter::{
    run_fibrefilter, AnalysisError as PipelineError, CvScheme, DisplayStrategy, FibreMode,
    FibreRunOptions, FibreSettings, FibreTest, Metric, PredBasis, H0,
};
use dbsloc_pipeline::group::netmap::{
    find_seed_maps, run_netmap, seed_map_naming, MapKind, MaskChoice, NetmapRunOptions,
    NetmapSettings,
};
use dbsloc_pipeline::group::record::{subject_id, AnalysisRef, EXPLORER_LAYOUTS};
use dbsloc_pipeline::group::stats_common::{CorrKind, Correction};
use dbsloc_pipeline::group::sweetspot::{
    run_sweetspot, AmplitudeCorrection, CorrelationType, CvStrategy, Level, Multiple,
    Normalisation, NullHypothesis, PredictionBasis, Preset, SweetspotOptions, SweetspotSettings,
    Test,
};
use dbsloc_pipeline::group::{GroupError, GroupRecord};
use dbsloc_pipeline::sha256::file_sha256_hex;
use dbsloc_viz::scene_layers::{FibreColouring, FibreLayer, MapLayer, MapMode};
use eframe::egui;
use serde_json::Value;

use crate::connectome_cache_ui::{check_consent, pre_load, CacheRun, ConsentFlow, ConsentRequest};
use crate::group_panel::GroupEnv;

pub const MIN_PATIENTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    SweetSpot,
    FibreFiltering,
    NetworkMapping,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::SweetSpot, Kind::FibreFiltering, Kind::NetworkMapping];

    fn index(self) -> usize {
        match self {
            Kind::SweetSpot => 0,
            Kind::FibreFiltering => 1,
            Kind::NetworkMapping => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::SweetSpot => "Sweet spot",
            Kind::FibreFiltering => "Fibre filtering",
            Kind::NetworkMapping => "Network mapping",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Kind::SweetSpot => {
                "Map the voxels where stimulation goes with a better outcome and test how well the map predicts it"
            }
            Kind::FibreFiltering => {
                "Score every fibre of a structural connectome by how well its stimulation goes with the outcome"
            }
            Kind::NetworkMapping => {
                "Correlate every voxel of the connectivity maps of the stimulation with the outcome"
            }
        }
    }

    pub fn record_kind(self) -> &'static str {
        EXPLORER_LAYOUTS[self.index()].0
    }

    pub fn folder(self) -> &'static str {
        EXPLORER_LAYOUTS[self.index()].1
    }

    pub fn json_name(self) -> &'static str {
        EXPLORER_LAYOUTS[self.index()].2
    }

    pub fn from_record_kind(kind: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.record_kind() == kind)
    }

    pub fn register(self, record: &mut GroupRecord, id: &str) {
        record.register_analysis(self.record_kind(), self.folder(), id, self.json_name());
    }

    fn needs_connectome(self) -> bool {
        self != Kind::SweetSpot
    }

    fn accepts(self, class: ConnectomeClass) -> bool {
        match self {
            Kind::SweetSpot => false,
            Kind::FibreFiltering => class != ConnectomeClass::Fmri,
            Kind::NetworkMapping => class != ConnectomeClass::DmriMultiTract,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvKind {
    LeaveOneOut,
    KFold,
    LeaveOneCohortOut,
    LeaveNothingOut,
}

impl CvKind {
    const ALL: [CvKind; 4] = [
        CvKind::LeaveOneOut,
        CvKind::KFold,
        CvKind::LeaveOneCohortOut,
        CvKind::LeaveNothingOut,
    ];

    fn label(self) -> &'static str {
        match self {
            CvKind::LeaveOneOut => "Leave one out",
            CvKind::KFold => "k-fold",
            CvKind::LeaveOneCohortOut => "Leave one cohort out",
            CvKind::LeaveNothingOut => "None, permutation test",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            CvKind::LeaveOneOut => "Predict each patient from a model fitted on all the others",
            CvKind::KFold => {
                "Split the patients into k folds and predict each fold from the others"
            }
            CvKind::LeaveOneCohortOut => {
                "Predict each group from a model fitted on the other groups"
            }
            CvKind::LeaveNothingOut => {
                "Fit on every patient and judge the fit against permuted outcomes"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CvChoice {
    pub kind: CvKind,
    pub k: usize,
    pub iterations: usize,
    pub permutations: usize,
    pub seed: u64,
}

impl Default for CvChoice {
    fn default() -> Self {
        CvChoice {
            kind: CvKind::LeaveOneOut,
            k: 5,
            iterations: 1,
            permutations: 1000,
            seed: 1,
        }
    }
}

impl CvChoice {
    pub fn scheme(&self) -> CvScheme {
        match self.kind {
            CvKind::LeaveOneOut => CvScheme::LeaveOneOut,
            CvKind::KFold => CvScheme::KFold {
                k: self.k,
                iterations: self.iterations,
            },
            CvKind::LeaveOneCohortOut => CvScheme::LeaveOneCohortOut,
            CvKind::LeaveNothingOut => CvScheme::LeaveNothingOut {
                permutations: self.permutations,
            },
        }
    }

    pub fn strategy(&self) -> CvStrategy {
        match self.kind {
            CvKind::LeaveOneOut => CvStrategy::LeaveOnePatientOut,
            CvKind::KFold => CvStrategy::KFold,
            CvKind::LeaveOneCohortOut => CvStrategy::LeaveOneCohortOut,
            CvKind::LeaveNothingOut => CvStrategy::LeaveNothingOut,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskPick {
    Grey,
    Brain,
    All,
}

impl MaskPick {
    fn choice(self) -> MaskChoice {
        match self {
            MaskPick::Grey => MaskChoice::Grey,
            MaskPick::Brain => MaskChoice::Brain,
            MaskPick::All => MaskChoice::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectomeEntry {
    pub spec: String,
    pub label: String,
    pub class: ConnectomeClass,
}

pub fn list_entries(roots: &ConnectomeRoots) -> Vec<ConnectomeEntry> {
    let mut out = Vec::new();
    for c in discover_connectomes(roots) {
        if c.missing || c.duplicate_of.is_some() {
            continue;
        }
        out.push(ConnectomeEntry {
            spec: c.id.clone(),
            label: format!("{} ({})", c.name, c.class.kind_label().to_lowercase()),
            class: c.class,
        });
        if c.class == ConnectomeClass::Fmri {
            for s in &c.subsets {
                out.push(ConnectomeEntry {
                    spec: format!("{} > {s}", c.id),
                    label: format!("{} > {s} (functional)", c.name),
                    class: c.class,
                });
            }
        }
    }
    out
}

pub fn eligible(kind: Kind, entries: &[ConnectomeEntry]) -> Vec<&ConnectomeEntry> {
    entries.iter().filter(|e| kind.accepts(e.class)).collect()
}

enum ListLoad {
    Unloaded,
    Loading(mpsc::Receiver<Vec<ConnectomeEntry>>),
    Ready(Vec<ConnectomeEntry>),
}

pub struct ConnectomeList {
    dir: Option<PathBuf>,
    load: ListLoad,
}

impl Default for ConnectomeList {
    fn default() -> Self {
        ConnectomeList {
            dir: None,
            load: ListLoad::Unloaded,
        }
    }
}

impl ConnectomeList {
    fn drive(&mut self, ctx: &egui::Context, dir: Option<&Path>) {
        if self.dir.as_deref() != dir {
            self.dir = dir.map(Path::to_path_buf);
            self.load = ListLoad::Unloaded;
        }
        match &self.load {
            ListLoad::Unloaded => {
                let (tx, rx) = mpsc::channel();
                let dir = self.dir.clone();
                let ctx = ctx.clone();
                thread::spawn(move || {
                    let entries = crate::connectome_roots::resolve(dir.as_deref())
                        .map(|r| list_entries(&r))
                        .unwrap_or_default();
                    let _ = tx.send(entries);
                    ctx.request_repaint();
                });
                self.load = ListLoad::Loading(rx);
            }
            ListLoad::Loading(rx) => match rx.try_recv() {
                Ok(entries) => self.load = ListLoad::Ready(entries),
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => self.load = ListLoad::Ready(Vec::new()),
            },
            ListLoad::Ready(_) => {}
        }
    }

    fn entries(&self) -> Option<&[ConnectomeEntry]> {
        match &self.load {
            ListLoad::Ready(e) => Some(e),
            _ => None,
        }
    }

    fn reload(&mut self) {
        self.load = ListLoad::Unloaded;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    pub kind: Kind,
    pub id: String,
    pub created: String,
    pub variable: String,
    pub n: Option<usize>,
    pub r: Option<f64>,
    pub p: Option<f64>,
    pub auc: Option<f64>,
    pub folder: PathBuf,
}

fn rfc_of_label(label: &str) -> String {
    match (
        label.get(0..4),
        label.get(4..6),
        label.get(6..8),
        label.get(8..10),
        label.get(10..12),
        label.get(12..14),
    ) {
        (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) => {
            format!("{y}-{mo}-{d}T{h}:{mi}:{s}")
        }
        _ => String::new(),
    }
}

pub fn parse_row(
    kind: Kind,
    id: &str,
    folder: PathBuf,
    value: Option<&Value>,
    modified: Option<u64>,
) -> ResultRow {
    let null = Value::Null;
    let v = value.unwrap_or(&null);
    let (stats, variable) = match kind {
        Kind::SweetSpot => (&v["cross_validation"], &v["outcome"]["variable"]),
        Kind::FibreFiltering => (&v["cross_validation"], &v["provenance"]["variable"]),
        Kind::NetworkMapping => (&v["results"], &v["provenance"]["variable"]),
    };
    let created = v["created_at"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            modified.map(|secs| rfc_of_label(&crate::stimulation_panel::timestamp_label(secs)))
        })
        .unwrap_or_default();
    ResultRow {
        kind,
        id: id.to_owned(),
        created,
        variable: variable.as_str().unwrap_or_default().to_owned(),
        n: stats["n"].as_u64().map(|n| n as usize),
        r: stats["r"].as_f64(),
        p: stats["p"].as_f64(),
        auc: stats["auc"].as_f64(),
        folder,
    }
}

pub fn rows_key(analyses: &[AnalysisRef]) -> Vec<(String, String)> {
    analyses
        .iter()
        .filter(|a| Kind::from_record_kind(&a.kind).is_some())
        .map(|a| (a.kind.clone(), a.id.clone()))
        .collect()
}

pub fn load_rows(group_folder: &Path, analyses: &[AnalysisRef]) -> Vec<ResultRow> {
    let mut rows: Vec<ResultRow> = Vec::new();
    for a in analyses {
        let Some(kind) = Kind::from_record_kind(&a.kind) else {
            continue;
        };
        let file = group_folder.join(&a.record);
        let folder = file
            .parent()
            .map_or_else(|| group_folder.to_path_buf(), Path::to_path_buf);
        let value: Option<Value> = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        let modified = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        rows.push(parse_row(kind, &a.id, folder, value.as_ref(), modified));
    }
    rows.sort_by(|a, b| (&b.created, &b.id).cmp(&(&a.created, &a.id)));
    rows
}

type RowsMsg = (Vec<(String, String)>, Vec<ResultRow>);

#[derive(Default)]
pub struct Results {
    key: Option<Vec<(String, String)>>,
    rows: Vec<ResultRow>,
    rx: Option<mpsc::Receiver<RowsMsg>>,
}

impl Results {
    fn drive(&mut self, ctx: &egui::Context, folder: &Path, analyses: &[AnalysisRef]) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok((key, rows)) => {
                    self.key = Some(key);
                    self.rows = rows;
                    self.rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.rx = None,
            }
        }
        let key = rows_key(analyses);
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let folder = folder.to_path_buf();
        let analyses = analyses.to_vec();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let rows = load_rows(&folder, &analyses);
            let _ = tx.send((rows_key(&analyses), rows));
            ctx.request_repaint();
        });
    }

    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &ResultRow> {
        self.rows.iter().filter(move |r| r.kind == kind)
    }
}

pub struct ExplorerState {
    pub kind: Kind,
    pub variable: Option<String>,
    pub covariates: Vec<String>,
    pub sweet: SweetspotSettings,
    pub fibre: FibreSettings,
    pub netmap: NetmapSettings,
    pub cv: CvChoice,
    pub fibre_mirror: bool,
    pub mask: MaskPick,
    pub connectome: HashMap<Kind, String>,
    pub connectomes: ConnectomeList,
    pub results: Results,
    pub confirm: Option<(Kind, String)>,
    pub remembered: Option<(String, String)>,
    pub open: bool,
    pub cache: CacheSetting,
    pub consent: ConsentFlow,
    pub pending: Option<Plan>,
    pub answered: Option<(Plan, CacheRun)>,
}

impl Default for ExplorerState {
    fn default() -> Self {
        ExplorerState {
            kind: Kind::SweetSpot,
            variable: None,
            covariates: Vec::new(),
            sweet: SweetspotSettings::default(),
            fibre: FibreSettings::default(),
            netmap: NetmapSettings::default(),
            cv: CvChoice::default(),
            fibre_mirror: false,
            mask: MaskPick::Grey,
            connectome: HashMap::new(),
            connectomes: ConnectomeList::default(),
            results: Results::default(),
            confirm: None,
            remembered: None,
            open: std::env::var_os("DBSLOC_APP_GROUP_ANALYSES").is_some(),
            cache: CacheSetting::default(),
            consent: ConsentFlow::default(),
            pending: None,
            answered: None,
        }
    }
}

pub fn display_percent(settings: &FibreSettings) -> f64 {
    match settings.display.strategy {
        DisplayStrategy::RelativeToPeak { percent } => percent,
        _ => 25.0,
    }
}

pub fn set_display_percent(settings: &mut FibreSettings, percent: f64) {
    settings.display.strategy = DisplayStrategy::RelativeToPeak { percent };
}

pub fn sweetspot_settings(state: &ExplorerState) -> SweetspotSettings {
    let mut s = state.sweet.clone();
    s.cv = state.cv.strategy();
    s.k_folds = state.cv.k;
    s.permutations = state.cv.permutations;
    s
}

pub fn sweetspot_options(state: &ExplorerState, folder: &Path, variable: &str) -> SweetspotOptions {
    SweetspotOptions {
        group_folder: folder.to_path_buf(),
        variable: variable.to_owned(),
        covariates: state.covariates.clone(),
        settings: sweetspot_settings(state),
        space: REFERENCE_SPACE.to_owned(),
        model: None,
        patients: None,
    }
}

pub fn fibre_options(
    state: &ExplorerState,
    folder: &Path,
    variable: &str,
    connectome: &str,
    root: Option<&Path>,
) -> FibreRunOptions {
    FibreRunOptions {
        variable: variable.to_owned(),
        covariates: state.covariates.clone(),
        connectome: connectome.to_owned(),
        connectome_root: root.map(Path::to_path_buf),
        space: REFERENCE_SPACE.to_owned(),
        model: None,
        settings: state.fibre.clone(),
        scheme: state.cv.scheme(),
        seed: state.cv.seed,
        mirror: state.fibre_mirror,
        group_folder: folder.to_path_buf(),
        cache: state.cache,
    }
}

pub fn netmap_options(
    state: &ExplorerState,
    folder: &Path,
    variable: &str,
    connectome: &str,
    root: Option<&Path>,
    atlases_dir: Option<&Path>,
) -> NetmapRunOptions {
    NetmapRunOptions {
        variable: variable.to_owned(),
        covariates: state.covariates.clone(),
        connectome: connectome.to_owned(),
        connectome_root: root.map(Path::to_path_buf),
        space: REFERENCE_SPACE.to_owned(),
        model: None,
        settings: state.netmap.clone(),
        mask: state.mask.choice(),
        atlases_dir: atlases_dir.map(Path::to_path_buf),
        scheme: state.cv.scheme(),
        seed: state.cv.seed,
        mirror: false,
        group_folder: folder.to_path_buf(),
    }
}

#[derive(Debug, Clone)]
pub enum Plan {
    Sweetspot(Box<SweetspotOptions>),
    Fibre(Box<FibreRunOptions>),
    Netmap(Box<NetmapRunOptions>),
}

pub fn plan(
    state: &ExplorerState,
    folder: &Path,
    variable: &str,
    connectome: Option<&str>,
    env: &GroupEnv,
) -> Option<Plan> {
    let root = env.connectomes_dir.as_deref();
    match state.kind {
        Kind::SweetSpot => Some(Plan::Sweetspot(Box::new(sweetspot_options(
            state, folder, variable,
        )))),
        Kind::FibreFiltering => Some(Plan::Fibre(Box::new(fibre_options(
            state,
            folder,
            variable,
            connectome?,
            root,
        )))),
        Kind::NetworkMapping => Some(Plan::Netmap(Box::new(netmap_options(
            state,
            folder,
            variable,
            connectome?,
            root,
            env.atlases_dir.as_deref(),
        )))),
    }
}

fn resolve_plan_connectome(root: Option<&Path>, spec: &str) -> Option<ResolvedConnectome> {
    let roots = crate::connectome_roots::resolve(root).ok()?;
    resolve_connectome(&roots, spec).ok()
}

pub fn consent_probe(record: &GroupRecord, plan: &Plan) -> Option<ConsentRequest> {
    match plan {
        Plan::Sweetspot(_) => None,
        Plan::Fibre(o) => {
            let conn = resolve_plan_connectome(o.connectome_root.as_deref(), &o.connectome)?;
            check_consent(&conn, CacheSetting::Ask)
        }
        Plan::Netmap(o) => {
            let conn = resolve_plan_connectome(o.connectome_root.as_deref(), &o.connectome)?;
            if conn.class != ConnectomeClass::Dmri {
                return None;
            }
            if plan_maps(record, o, &conn, &mut Vec::new()).is_empty() {
                return None;
            }
            check_consent(&conn, CacheSetting::Ask)
        }
    }
}

pub fn plan_asks(plan: &Plan, setting: CacheSetting) -> bool {
    setting == CacheSetting::Ask && !matches!(plan, Plan::Sweetspot(_))
}

pub struct Gate<'a> {
    pub variables: &'a [String],
    pub included: usize,
    pub computed: usize,
    pub busy: bool,
    pub connectomes: Option<&'a [ConnectomeEntry]>,
}

pub fn run_block_reason(
    kind: Kind,
    variable: Option<&str>,
    connectome: Option<&str>,
    gate: &Gate<'_>,
) -> Option<&'static str> {
    if gate.busy {
        return Some("Wait for the running job to finish");
    }
    if gate.variables.is_empty() {
        return Some("Import clinical variables first, see Clinical variables");
    }
    if variable.is_none() {
        return Some("Choose the outcome variable");
    }
    if gate.included < MIN_PATIENTS {
        return Some("At least 3 included patients are needed");
    }
    if gate.computed < MIN_PATIENTS {
        return Some("At least 3 patients need computed VTAs; use Compute VTAs");
    }
    if kind.needs_connectome() {
        let Some(entries) = gate.connectomes else {
            return Some("Looking for installed connectomes");
        };
        let usable = eligible(kind, entries);
        if usable.is_empty() {
            return Some("No matching connectome is installed; use Connectomes…");
        }
        if !connectome.is_some_and(|c| usable.iter().any(|e| e.spec == c)) {
            return Some("Choose a connectome");
        }
    }
    None
}

pub fn summary_line(
    kind: Kind,
    id: &str,
    scheme: &str,
    r: f64,
    p: f64,
    auc: Option<f64>,
    n: usize,
) -> String {
    let auc = auc.map_or_else(String::new, |a| format!(", AUC {a:.2}"));
    format!(
        "group: {} {id}: {scheme} r = {r:.3} (p = {}{auc}, n = {n})",
        kind.label().to_lowercase(),
        format_p(p)
    )
}

pub fn format_p(p: f64) -> String {
    if p < 0.001 {
        "<0.001".to_owned()
    } else {
        format!("{p:.3}")
    }
}

fn stat(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{v:.2}"))
}

pub fn row_text(row: &ResultRow) -> String {
    let date = row.created.get(..10).unwrap_or_default();
    let mut text = format!(
        "{} · {} · n {} · r {} · p {}",
        date,
        if row.variable.is_empty() {
            "-"
        } else {
            row.variable.as_str()
        },
        row.n.map_or_else(|| "-".to_owned(), |n| n.to_string()),
        stat(row.r),
        row.p.map_or_else(|| "-".to_owned(), format_p),
    );
    if let Some(a) = row.auc {
        text.push_str(&format!(" · AUC {a:.2}"));
    }
    text
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub done: usize,
    pub total: usize,
    pub text: String,
}

impl Update {
    fn text(text: &str) -> Update {
        Update {
            done: 0,
            total: 0,
            text: text.to_owned(),
        }
    }
}

pub type Sink<'a> = &'a (dyn Fn(Update) + Sync);

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisDone {
    pub kind: Kind,
    pub id: String,
    pub summary: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisFail {
    Cancelled,
    Failed(String),
}

fn from_group(e: GroupError) -> AnalysisFail {
    match e {
        GroupError::Lg {
            code: "cancelled", ..
        } => AnalysisFail::Cancelled,
        other => AnalysisFail::Failed(other.to_string()),
    }
}

fn from_pipeline(e: PipelineError) -> AnalysisFail {
    match e {
        PipelineError::Cancelled => AnalysisFail::Cancelled,
        PipelineError::Group(g) => from_group(g),
        PipelineError::Map(m) => from_map(m),
    }
}

fn from_map(e: MapError) -> AnalysisFail {
    match e {
        MapError::Cancelled => AnalysisFail::Cancelled,
        other => AnalysisFail::Failed(other.to_string()),
    }
}

fn progress_update(p: &Progress) -> Update {
    Update {
        done: usize::try_from(p.done).unwrap_or(0),
        total: usize::try_from(p.total).unwrap_or(0),
        text: if p.message.is_empty() {
            p.stage.clone()
        } else {
            format!("{} · {}", p.stage, p.message)
        },
    }
}

fn sidecar_of(map: &Path) -> PathBuf {
    let name = map
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = [".nii.gz", ".nii"]
        .iter()
        .find_map(|e| name.strip_suffix(e))
        .unwrap_or(&name)
        .to_owned();
    map.with_file_name(format!("{stem}.json"))
}

pub fn map_is_stale(sidecar: &Value, digest: &str, seed_kind: &str, grid: Option<&str>) -> bool {
    if sidecar["connectome"]["digest"]
        .as_str()
        .is_some_and(|d| d != digest)
    {
        return true;
    }
    if sidecar["seed"]["kind"]
        .as_str()
        .is_some_and(|k| k != seed_kind)
    {
        return true;
    }
    grid.is_some_and(|g| sidecar["seed"]["grid"].as_str().is_some_and(|x| x != g))
}

fn inputs_changed(sidecar: &Value) -> bool {
    sidecar["inputs"].as_array().is_some_and(|rows| {
        rows.iter().any(|r| {
            let (Some(file), Some(sha)) = (r["file"].as_str(), r["sha256"].as_str()) else {
                return false;
            };
            let path = Path::new(file);
            !path.is_file() || file_sha256_hex(path).is_ok_and(|h| h != sha)
        })
    })
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

pub fn map_needs_run(map: &Path, digest: &str, seed_kind: &str, grid: Option<&str>) -> bool {
    let Some(sidecar) = read_json(&sidecar_of(map)) else {
        return false;
    };
    if map_is_stale(&sidecar, digest, seed_kind, grid) || inputs_changed(&sidecar) {
        return true;
    }
    sidecar["seed"]["file"]
        .as_str()
        .and_then(|f| read_json(&sidecar_of(Path::new(f))))
        .is_some_and(|seed| inputs_changed(&seed))
}

struct MapJob {
    subject: String,
    root: PathBuf,
}

fn plan_maps(
    record: &GroupRecord,
    opts: &NetmapRunOptions,
    conn: &ResolvedConnectome,
    notes: &mut Vec<String>,
) -> Vec<MapJob> {
    let (seed_kind, token, suffix) = seed_map_naming(conn);
    let grid = (conn.class != ConnectomeClass::Fmri).then(|| OutputGrid::Mm2.name());
    let label = record.stim_label();
    let mut todo = Vec::new();
    let (mut kept, mut stale, mut as_is) = (0usize, 0usize, 0usize);
    for p in record.patients.iter().filter(|p| p.included) {
        let Some(root) = record.derivatives_root_of(p) else {
            notes.push(format!("{}: no derivatives folder, left out", p.subject));
            continue;
        };
        let sid = subject_id(&p.subject);
        let dir = stimulation_folder(Path::new(root), sid, &opts.space, &label);
        if find_side_files(&dir, sid, SeedKind::Binary, opts.model.as_deref()).is_err() {
            notes.push(format!("{}: no computed VTA, left out", p.subject));
            continue;
        }
        let (plain, _) =
            find_seed_maps(&dir, sid, seed_kind, &token, suffix, opts.model.as_deref());
        let needs = match &plain {
            None => true,
            Some(f) => {
                let s = map_needs_run(f, &conn.digest, SeedKind::Binary.name(), grid);
                stale += usize::from(s);
                as_is += usize::from(!sidecar_of(f).is_file());
                s
            }
        };
        if needs {
            todo.push(MapJob {
                subject: sid.to_owned(),
                root: PathBuf::from(root),
            });
        } else {
            kept += 1;
        }
    }
    if kept > 0 || stale > 0 {
        notes.push(format!(
            "connectivity maps: {kept} up to date, {stale} recomputed because the stimulation or the connectome changed"
        ));
    }
    if as_is > 0 {
        notes.push(format!(
            "connectivity maps: {as_is} without a sidecar file (for example made by Lead-DBS) used as they are"
        ));
    }
    todo
}

fn note_outcome(o: &SubjectOutcome, notes: &mut Vec<String>) {
    match &o.status {
        Status::Ok => {}
        Status::Skipped(r) => notes.push(format!("sub-{}: {r}", o.subject_id)),
        Status::Error { code, message } => {
            notes.push(format!("sub-{}: {code} {message}", o.subject_id));
        }
    }
    for w in &o.warnings {
        notes.push(format!("sub-{}: {w}", o.subject_id));
    }
}

fn prepare_maps(
    record: &GroupRecord,
    opts: &NetmapRunOptions,
    run: &CacheRun,
    cancel: &AtomicBool,
    sink: Sink<'_>,
) -> Result<Vec<String>, AnalysisFail> {
    let roots = crate::connectome_roots::resolve(opts.connectome_root.as_deref())
        .map_err(|e| AnalysisFail::Failed(e.to_string()))?;
    let conn = resolve_connectome(&roots, &opts.connectome).map_err(from_map)?;
    let mut notes: Vec<String> = Vec::new();
    let todo = plan_maps(record, opts, &conn, &mut notes);
    if todo.is_empty() {
        return Ok(notes);
    }
    let label = record.stim_label();
    let total = todo.len();
    sink(Update::text("Loading the connectome"));
    if conn.class == ConnectomeClass::Fmri {
        let functional =
            FunctionalConnectome::open(&conn.path).map_err(|e| from_map(MapError::from(e)))?;
        let reqs: Vec<FunctionalRequest> = todo
            .iter()
            .map(|j| FunctionalRequest {
                derivatives_root: j.root.clone(),
                subject_id: j.subject.clone(),
                label: label.clone(),
                space: opts.space.clone(),
                connectome: conn.clone(),
                kind: SeedKind::Binary,
                model: opts.model.clone(),
                seed_file: None,
                per_subject_maps: false,
            })
            .collect();
        let outcomes = run_functional_batch(
            &functional,
            &reqs,
            &FunctionalPass::default(),
            cancel,
            &|p: &Progress| {
                let mut u = progress_update(p);
                u.text = format!("Connectivity maps · {}", u.text);
                sink(u);
            },
        )
        .map_err(from_map)?;
        for o in &outcomes {
            note_outcome(o, &mut notes);
        }
        return Ok(notes);
    }
    let load_progress = |p: &Progress| sink(progress_update(p));
    let prepared = pre_load(&conn, run, cancel, &load_progress).map_err(from_map)?;
    let engine = StructuralEngine::load_with(
        &conn,
        &LoadOptions {
            setting: prepared.setting,
            cancel: Some(cancel),
            progress: Some(&load_progress),
        },
    )
    .map_err(from_map)?;
    notes.extend(prepared.warnings);
    notes.extend(engine.notes.iter().cloned());
    notes.extend(engine.warnings.iter().cloned());
    for (i, job) in todo.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AnalysisFail::Cancelled);
        }
        let name = format!("Connectivity maps · sub-{}", job.subject);
        sink(Update {
            done: i,
            total,
            text: name.clone(),
        });
        let req = StructuralRequest {
            derivatives_root: job.root.clone(),
            subject_id: job.subject.clone(),
            label: label.clone(),
            space: opts.space.clone(),
            connectome: conn.clone(),
            kind: SeedKind::Binary,
            model: opts.model.clone(),
            grid: OutputGrid::Mm2,
            seed_file: None,
            write_subset: false,
        };
        let result = run_structural(&engine, &req, cancel, &|p: &Progress| {
            sink(Update {
                done: i,
                total,
                text: format!("{name} · {}", p.stage),
            });
        });
        match SubjectOutcome::from_result(&job.subject, result) {
            Ok(o) => note_outcome(&o, &mut notes),
            Err(_) => return Err(AnalysisFail::Cancelled),
        }
    }
    Ok(notes)
}

pub fn run_plan(
    record: &GroupRecord,
    plan: &Plan,
    run: &CacheRun,
    cancel: &AtomicBool,
    sink: Sink<'_>,
) -> Result<AnalysisDone, AnalysisFail> {
    match plan {
        Plan::Sweetspot(o) => {
            let report = run_sweetspot(record, o, cancel, &|t: &str| sink(Update::text(t)))
                .map_err(from_group)?;
            Ok(AnalysisDone {
                kind: Kind::SweetSpot,
                id: report.id.clone(),
                summary: summary_line(
                    Kind::SweetSpot,
                    &report.id,
                    o.settings.cv.name(),
                    report.cv.r,
                    report.cv.p,
                    report.cv.auc,
                    report.cv.n_samples,
                ),
                notes: report.notes.iter().map(ToString::to_string).collect(),
            })
        }
        Plan::Fibre(o) => {
            let progress = |p: &Progress| sink(progress_update(p));
            let mut opts = (**o).clone();
            let mut notes: Vec<String> = Vec::new();
            match resolve_plan_connectome(opts.connectome_root.as_deref(), &opts.connectome) {
                Some(conn) => {
                    let prepared = pre_load(&conn, run, cancel, &progress).map_err(from_map)?;
                    opts.cache = prepared.setting;
                    notes = prepared.warnings;
                }
                None => opts.cache = run.setting,
            }
            let run = run_fibrefilter(record, &opts, cancel, &progress).map_err(from_pipeline)?;
            let ev = run.result.evaluation;
            Ok(AnalysisDone {
                kind: Kind::FibreFiltering,
                id: run.outputs.id.clone(),
                summary: summary_line(
                    Kind::FibreFiltering,
                    &run.outputs.id,
                    o.scheme.name(),
                    ev.r,
                    ev.p,
                    ev.auc,
                    ev.n,
                ),
                notes: notes
                    .into_iter()
                    .chain(run.notes.iter().map(ToString::to_string))
                    .collect(),
            })
        }
        Plan::Netmap(o) => {
            let mut notes = prepare_maps(record, o, run, cancel, sink)?;
            let run = run_netmap(record, o, cancel, &|p: &Progress| sink(progress_update(p)))
                .map_err(from_pipeline)?;
            let ev = run.result.evaluation;
            notes.extend(run.notes.iter().map(ToString::to_string));
            Ok(AnalysisDone {
                kind: Kind::NetworkMapping,
                id: run.outputs.id.clone(),
                summary: summary_line(
                    Kind::NetworkMapping,
                    &run.outputs.id,
                    o.scheme.name(),
                    ev.r,
                    ev.p,
                    ev.auc,
                    ev.n,
                ),
                notes,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GroupLayer {
    Fibre(FibreLayer),
    Map(MapLayer),
}

pub fn layer_ids(kind: Kind, id: &str) -> Vec<String> {
    match kind {
        Kind::SweetSpot => ["R", "L"].iter().map(|s| format!("ss:{id}:{s}")).collect(),
        Kind::FibreFiltering => ["R", "L"].iter().map(|s| format!("ff:{id}:{s}")).collect(),
        Kind::NetworkMapping => vec![format!("nm:{id}")],
    }
}

pub fn pick_map_files(kind: Kind, names: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match kind {
        Kind::SweetSpot => {
            for side in ["R", "L"] {
                let tag = format!("_hemi-{side}_");
                let found = names
                    .iter()
                    .filter(|n| {
                        n.ends_with("_map.nii.gz")
                            && !n.contains("_desc-coverage")
                            && !n.contains("_group-")
                            && n.contains(&tag)
                    })
                    .min();
                if let Some(n) = found {
                    out.push((side.to_owned(), n.clone()));
                }
            }
        }
        Kind::NetworkMapping => {
            if let Some(n) = names.iter().filter(|n| n.ends_with("_map.nii.gz")).min() {
                out.push((String::new(), n.clone()));
            }
        }
        Kind::FibreFiltering => {}
    }
    out
}

pub fn load_layers(group_folder: &Path, kind: Kind, id: &str) -> Result<Vec<GroupLayer>, String> {
    let dir = group_folder.join(kind.folder()).join(id);
    let ids = layer_ids(kind, id);
    let mut out = Vec::new();
    if kind == Kind::FibreFiltering {
        for (side, layer_id) in ["R", "L"].iter().zip(ids) {
            let file = dir.join(format!("fibres_{side}.mat"));
            if !file.is_file() {
                continue;
            }
            let set = read_fibres(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let n = set.n_fibres();
            let mut values = set.vals;
            if values.len() != n {
                values = vec![1.0; n];
            }
            out.push(GroupLayer::Fibre(FibreLayer {
                id: layer_id,
                label: format!("Fibres {side} · {id}"),
                points: set.points,
                offsets: set.fibre_offsets,
                values,
                colouring: FibreColouring::Signed,
                total_fibres: n,
            }));
        }
        if out.is_empty() {
            return Err("no fibre was kept; raise Display percent or change the test".to_owned());
        }
        return Ok(out);
    }
    let names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for (side, name) in pick_map_files(kind, &names) {
        let vol = read_nii(&dir.join(&name)).map_err(|e| format!("{name}: {e}"))?;
        let (layer_id, label) = if side.is_empty() {
            (format!("nm:{id}"), format!("Network map · {id}"))
        } else {
            (
                format!("ss:{id}:{side}"),
                format!("Sweet spot {side} · {id}"),
            )
        };
        out.push(GroupLayer::Map(MapLayer::new(
            layer_id,
            label,
            vol.dims,
            vol.affine,
            vol.data,
            MapMode::Isosurface,
        )));
    }
    if out.is_empty() {
        return Err(format!("no map file in {}", dir.display()));
    }
    Ok(out)
}

pub fn delete_analysis(
    group_folder: &Path,
    record: &mut GroupRecord,
    kind: Kind,
    id: &str,
) -> Result<(), String> {
    let folder = group_folder.join(kind.folder()).join(id);
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    if id.is_empty() || !id.chars().all(plain) {
        return Err(format!("refusing to delete {}", folder.display()));
    }
    if folder.exists() {
        std::fs::remove_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    }
    record
        .analyses
        .retain(|a| !(a.kind == kind.record_kind() && a.id == id));
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Run,
    Show(Kind, String),
    OpenFolder(PathBuf),
    Delete(Kind, String),
}

pub struct Cx<'a> {
    pub record: &'a GroupRecord,
    pub env: &'a GroupEnv,
    pub folder: &'a Path,
    pub busy: bool,
    pub included: usize,
    pub computed: usize,
}

type Pal = crate::app::tokens::Palette;

fn pretty(name: &str) -> String {
    let text = name.replace('-', " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn named<T: Copy>(all: &[T], name: impl Fn(T) -> &'static str) -> Vec<(T, String)> {
    all.iter().map(|v| (*v, pretty(name(*v)))).collect()
}

fn combo_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    tip: &str,
    value: &mut T,
    options: &[(T, String)],
) -> bool {
    let before = *value;
    let l = ui.label(label);
    let current = options
        .iter()
        .find(|(v, _)| v == value)
        .map_or_else(String::new, |(_, t)| t.clone());
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (v, t) in options {
                ui.selectable_value(value, *v, t.as_str());
            }
        })
        .response
        .labelled_by(l.id)
        .on_hover_text(tip);
    ui.end_row();
    *value != before
}

fn f64_row(
    ui: &mut egui::Ui,
    label: &str,
    tip: &str,
    value: &mut f64,
    range: RangeInclusive<f64>,
    speed: f64,
    suffix: &str,
) {
    let l = ui.label(label);
    ui.add(
        egui::DragValue::new(value)
            .range(range)
            .speed(speed)
            .suffix(suffix),
    )
    .labelled_by(l.id)
    .on_hover_text(tip);
    ui.end_row();
}

fn usize_row(
    ui: &mut egui::Ui,
    label: &str,
    tip: &str,
    value: &mut usize,
    range: RangeInclusive<usize>,
) {
    let l = ui.label(label);
    ui.add(egui::DragValue::new(value).range(range))
        .labelled_by(l.id)
        .on_hover_text(tip);
    ui.end_row();
}

fn seed_row(ui: &mut egui::Ui, value: &mut u64) {
    let l = ui.label("Random seed");
    ui.add(egui::DragValue::new(value))
        .labelled_by(l.id)
        .on_hover_text(
            "Seed of the random folds and permutations; the same seed repeats the result",
        );
    ui.end_row();
}

fn check_row(ui: &mut egui::Ui, label: &str, tip: &str, value: &mut bool) {
    let l = ui.label(label);
    ui.add(egui::Checkbox::without_text(value))
        .labelled_by(l.id)
        .on_hover_text(tip);
    ui.end_row();
}

fn preset_label(p: Preset) -> &'static str {
    match p {
        Preset::Butson2011 => "Butson 2011",
        Preset::Cheung2011 => "Cheung 2011",
        Preset::Eisenstein2014 => "Eisenstein 2014",
        Preset::Reich2019 => "Reich 2019",
        Preset::Dembek2019 => "Dembek 2019",
        Preset::Elias2020 => "Elias 2020",
    }
}

pub fn current_preset(s: &SweetspotSettings) -> Option<Preset> {
    Preset::ALL.iter().copied().find(|p| {
        let mut t = SweetspotSettings::default();
        t.apply_preset(*p);
        s.level == Level::Vtas
            && s.test == t.test
            && s.null_hypothesis == t.null_hypothesis
            && s.improvement_threshold == t.improvement_threshold
            && s.n_threshold_percent == t.n_threshold_percent
            && s.amplitude_correction == t.amplitude_correction
    })
}

pub fn sweet_tests(level: Level) -> Vec<Test> {
    Test::ALL
        .iter()
        .copied()
        .filter(|t| match level {
            Level::Vtas => !matches!(t, Test::Correlations | Test::ReverseTTests),
            Level::EFields => matches!(t, Test::Correlations | Test::ReverseTTests),
        })
        .collect()
}

fn fibre_test_label(t: FibreTest) -> &'static str {
    match t {
        FibreTest::Spearman => "Spearman correlation",
        FibreTest::Pearson => "Pearson correlation",
        FibreTest::Bend => "Percentage-bend correlation",
        FibreTest::TwoSampleT => "Two-sample t-test",
        FibreTest::OneSampleT => "One-sample t-test",
        FibreTest::RankSum => "Rank-sum test",
        FibreTest::SignedRank => "Signed-rank test",
        FibreTest::ReverseT => "Reverse t-test",
        FibreTest::Proportion => "Proportion test",
        FibreTest::WeightedTwoSample => "Weighted two-sample t-test",
        FibreTest::NMap => "N-map",
        FibreTest::MeanMap => "Mean map",
    }
}

const FIBRE_TESTS: [FibreTest; 12] = [
    FibreTest::Spearman,
    FibreTest::Pearson,
    FibreTest::Bend,
    FibreTest::TwoSampleT,
    FibreTest::OneSampleT,
    FibreTest::RankSum,
    FibreTest::SignedRank,
    FibreTest::ReverseT,
    FibreTest::Proportion,
    FibreTest::WeightedTwoSample,
    FibreTest::NMap,
    FibreTest::MeanMap,
];

fn significance_options() -> Vec<(Option<Correction>, String)> {
    vec![
        (None, "Off".to_owned()),
        (Some(Correction::Uncorrected), "Uncorrected".to_owned()),
        (Some(Correction::Fdr), "FDR".to_owned()),
        (Some(Correction::Bonferroni), "Bonferroni".to_owned()),
    ]
}

fn corr_options() -> Vec<(CorrKind, String)> {
    vec![
        (CorrKind::Spearman, "Spearman".to_owned()),
        (CorrKind::Pearson, "Pearson".to_owned()),
        (CorrKind::Bend, "Percentage bend".to_owned()),
    ]
}

fn cv_rows(ui: &mut egui::Ui, cv: &mut CvChoice) {
    let options: Vec<(CvKind, String)> = CvKind::ALL
        .iter()
        .map(|k| (*k, k.label().to_owned()))
        .collect();
    combo_row(
        ui,
        "explorer_cv",
        "Cross-validation",
        "How the predictive power of the result is judged",
        &mut cv.kind,
        &options,
    );
    ui.label("");
    ui.add(
        egui::Label::new(egui::RichText::new(cv.kind.tip()).small().weak())
            .wrap()
            .selectable(false),
    );
    ui.end_row();
    match cv.kind {
        CvKind::KFold => {
            usize_row(ui, "Folds", "Number of folds k", &mut cv.k, 2..=50);
            usize_row(
                ui,
                "Repeats",
                "How many times the folds are drawn again and the result averaged",
                &mut cv.iterations,
                1..=100,
            );
        }
        CvKind::LeaveNothingOut => usize_row(
            ui,
            "Permutations",
            "Number of permuted outcomes of the permutation test",
            &mut cv.permutations,
            1..=100_000,
        ),
        _ => {}
    }
}

fn sweet_rows(state: &mut ExplorerState, ui: &mut egui::Ui) {
    let mut preset = current_preset(&state.sweet);
    let mut options: Vec<(Option<Preset>, String)> = vec![(None, "Custom".to_owned())];
    options.extend(
        Preset::ALL
            .iter()
            .map(|p| (Some(*p), preset_label(*p).to_owned())),
    );
    if combo_row(
        ui,
        "explorer_preset",
        "Preset",
        "Test and thresholds of a published study; every setting stays editable",
        &mut preset,
        &options,
    ) {
        if let Some(p) = preset {
            state.sweet.level = Level::Vtas;
            state.sweet.apply_preset(p);
        }
    }
    let tests: Vec<(Test, String)> = sweet_tests(state.sweet.level)
        .into_iter()
        .map(|t| (t, pretty(t.name())))
        .collect();
    combo_row(
        ui,
        "explorer_sweet_test",
        "Test",
        "The statistic computed in every voxel",
        &mut state.sweet.test,
        &tests,
    );
}

fn sweet_advanced(state: &mut ExplorerState, ui: &mut egui::Ui) {
    let s = &mut state.sweet;
    if combo_row(
        ui,
        "explorer_level",
        "Level",
        "Binary VTAs or the electric field values",
        &mut s.level,
        &named(Level::ALL, Level::name),
    ) {
        if s.level == Level::EFields {
            s.test = Test::Correlations;
        } else {
            s.test = Test::TTest;
            s.basis = PredictionBasis::MeanOfScores;
        }
    }
    combo_row(
        ui,
        "explorer_h0",
        "Null hypothesis",
        "What the one-group tests compare against",
        &mut s.null_hypothesis,
        &named(NullHypothesis::ALL, NullHypothesis::name),
    );
    f64_row(
        ui,
        "Improvement threshold",
        "Improvement that counts as a response in the threshold null hypothesis and the N-image",
        &mut s.improvement_threshold,
        -1000.0..=1000.0,
        0.01,
        "",
    );
    f64_row(
        ui,
        "N threshold",
        "Share of patients of the N-image test",
        &mut s.n_threshold_percent,
        0.0..=100.0,
        0.5,
        " %",
    );
    combo_row(
        ui,
        "explorer_amplitude",
        "Amplitude correction",
        "Remove the effect of stimulation amplitude or VTA size from the outcome",
        &mut s.amplitude_correction,
        &named(AmplitudeCorrection::ALL, AmplitudeCorrection::name),
    );
    combo_row(
        ui,
        "explorer_normalisation",
        "Normalisation",
        "Transform the outcome before the test",
        &mut s.normalisation,
        &named(Normalisation::ALL, Normalisation::name),
    );
    combo_row(
        ui,
        "explorer_correlation",
        "Correlation",
        "Correlation of the Correlations test",
        &mut s.correlation,
        &named(CorrelationType::ALL, CorrelationType::name),
    );
    combo_row(
        ui,
        "explorer_eval_correlation",
        "Evaluation correlation",
        "Correlation between observed and predicted outcome",
        &mut s.evaluation_correlation,
        &named(CorrelationType::ALL, CorrelationType::name),
    );
    f64_row(
        ui,
        "E-field threshold",
        "Field strength above which a voxel counts as covered",
        &mut s.efield_threshold,
        0.0..=10_000.0,
        1.0,
        " V/m",
    );
    f64_row(
        ui,
        "Missing below",
        "Field values below this become missing at the level E-fields",
        &mut s.nan_threshold,
        0.0..=10_000.0,
        1.0,
        " V/m",
    );
    f64_row(
        ui,
        "Coverage",
        "Share of patients that must cover a voxel for it to be tested",
        &mut s.coverage_percent,
        0.0..=100.0,
        0.5,
        " %",
    );
    f64_row(
        ui,
        "Alpha",
        "Significance level",
        &mut s.alpha,
        0.0001..=1.0,
        0.001,
        "",
    );
    combo_row(
        ui,
        "explorer_multiple",
        "Multiple comparisons",
        "Correction of the voxel p values",
        &mut s.multiple_comparisons,
        &named(Multiple::ALL, Multiple::name),
    );
    check_row(
        ui,
        "Significant only",
        "Show only the voxels that pass the significance filter",
        &mut s.show_significant_only,
    );
    check_row(
        ui,
        "Show sweet spot",
        "Show the positive part of the map",
        &mut s.show_positive,
    );
    check_row(
        ui,
        "Show sour spot",
        "Show the negative part of the map",
        &mut s.show_negative,
    );
    check_row(
        ui,
        "Mirror sides",
        "Use the flipped copies of both hemispheres as extra patients",
        &mut s.mirror,
    );
    check_row(
        ui,
        "Map per group",
        "Also write one map per group",
        &mut s.split_by_group,
    );
    combo_row(
        ui,
        "explorer_basis",
        "Prediction basis",
        "How a patient's predicted outcome is formed from the map",
        &mut s.basis,
        &named(PredictionBasis::ALL, PredictionBasis::name),
    );
    f64_row(
        ui,
        "Resolution",
        "Voxel size of the common grid",
        &mut s.resolution,
        0.1..=5.0,
        0.05,
        " mm",
    );
    seed_row(ui, &mut s.seed);
}

fn fibre_rows(state: &mut ExplorerState, ui: &mut egui::Ui) {
    let tests: Vec<(FibreTest, String)> = FIBRE_TESTS
        .iter()
        .map(|t| (*t, fibre_test_label(*t).to_owned()))
        .collect();
    combo_row(
        ui,
        "explorer_fibre_test",
        "Test",
        "The statistic computed for every fibre",
        &mut state.fibre.test,
        &tests,
    );
    let mut percent = display_percent(&state.fibre);
    f64_row(
        ui,
        "Display percent",
        "Show the fibres whose score is within this percentage of the best score",
        &mut percent,
        0.0..=100.0,
        0.5,
        " %",
    );
    set_display_percent(&mut state.fibre, percent);
}

fn fibre_advanced(state: &mut ExplorerState, ui: &mut egui::Ui) {
    let s = &mut state.fibre;
    combo_row(
        ui,
        "explorer_fibre_mode",
        "Value model",
        "What each patient contributes per fibre",
        &mut s.mode,
        &[
            (FibreMode::Vta, "VTA, binary".to_owned()),
            (FibreMode::Efield, "E-field".to_owned()),
            (FibreMode::Sigmoid, "Sigmoid of the E-field".to_owned()),
        ],
    );
    combo_row(
        ui,
        "explorer_fibre_metric",
        "Per-fibre value",
        "How the field values along a fibre are summarised",
        &mut s.metric,
        &[
            (Metric::Bin, "Binary".to_owned()),
            (Metric::Sum, "Sum".to_owned()),
            (Metric::Mean, "Mean".to_owned()),
            (Metric::Peak, "Peak".to_owned()),
            (Metric::Peak5, "Mean of the top 5".to_owned()),
        ],
    );
    combo_row(
        ui,
        "explorer_fibre_h0",
        "Null hypothesis",
        "What the one-group tests compare against",
        &mut s.h0,
        &[
            (H0::Average, "Average".to_owned()),
            (H0::Zero, "Zero".to_owned()),
        ],
    );
    f64_row(
        ui,
        "E-field threshold",
        "Field strength above which a fibre counts as stimulated",
        &mut s.efield_threshold,
        0.0..=10_000.0,
        1.0,
        " V/m",
    );
    f64_row(
        ui,
        "Coverage",
        "Share of patients in which a fibre must be connected",
        &mut s.coverage_percent,
        0.0..=100.0,
        0.5,
        " %",
    );
    combo_row(
        ui,
        "explorer_fibre_significance",
        "Significance filter",
        "Keep only fibres that pass this correction",
        &mut s.significance,
        &significance_options(),
    );
    f64_row(
        ui,
        "Alpha",
        "Significance level",
        &mut s.alpha,
        0.0001..=1.0,
        0.001,
        "",
    );
    combo_row(
        ui,
        "explorer_fibre_basis",
        "Prediction basis",
        "How a patient's predicted outcome is formed from the fibre scores",
        &mut s.basis,
        &[
            (PredBasis::Mean, "Mean".to_owned()),
            (PredBasis::Sum, "Sum".to_owned()),
            (PredBasis::Peak, "Peak".to_owned()),
            (PredBasis::Peak5, "Mean of the top 5".to_owned()),
        ],
    );
    check_row(
        ui,
        "Show positive",
        "Show the fibres with positive scores",
        &mut s.display.show_positive,
    );
    check_row(
        ui,
        "Show negative",
        "Show the fibres with negative scores",
        &mut s.display.show_negative,
    );
    check_row(
        ui,
        "Mirror sides",
        "Also use the flipped stimulation images",
        &mut state.fibre_mirror,
    );
    seed_row(ui, &mut state.cv.seed);
}

fn netmap_rows(state: &mut ExplorerState, ui: &mut egui::Ui) {
    combo_row(
        ui,
        "explorer_map",
        "Map",
        "The voxel-wise map computed over the patients",
        &mut state.netmap.map,
        &[
            (MapKind::RMap, "R-map".to_owned()),
            (MapKind::AMap, "A-map".to_owned()),
            (MapKind::CMap, "C-map".to_owned()),
            (MapKind::NMap, "N-map".to_owned()),
            (MapKind::TMap, "T-map".to_owned()),
        ],
    );
    combo_row(
        ui,
        "explorer_netmap_corr",
        "Correlation",
        "Correlation of the R-map and of the similarity prediction",
        &mut state.netmap.corr,
        &corr_options(),
    );
}

fn netmap_advanced(state: &mut ExplorerState, ui: &mut egui::Ui) {
    let s = &mut state.netmap;
    f64_row(
        ui,
        "N-map threshold",
        "Connectivity above this value counts in the N-map",
        &mut s.n_threshold,
        0.0..=10.0,
        0.01,
        "",
    );
    check_row(
        ui,
        "Show positive",
        "Show the positive part of the map",
        &mut s.show_positive,
    );
    check_row(
        ui,
        "Show negative",
        "Show the negative part of the map",
        &mut s.show_negative,
    );
    combo_row(
        ui,
        "explorer_netmap_significance",
        "Significance filter",
        "Keep only voxels that pass this correction",
        &mut s.significance,
        &significance_options(),
    );
    f64_row(
        ui,
        "Alpha",
        "Significance level",
        &mut s.alpha,
        0.0001..=1.0,
        0.001,
        "",
    );
    check_row(
        ui,
        "Fisher z",
        "Apply the Fisher z transform to the similarity",
        &mut s.fisher_z,
    );
    combo_row(
        ui,
        "explorer_mask",
        "Mask",
        "The voxels the similarity to the map is computed in",
        &mut state.mask,
        &[
            (MaskPick::Grey, "Grey matter".to_owned()),
            (MaskPick::Brain, "Brain".to_owned()),
            (MaskPick::All, "Whole volume".to_owned()),
        ],
    );
    seed_row(ui, &mut state.cv.seed);
}

impl ExplorerState {
    fn normalise(&mut self, variables: &[String]) {
        if !self
            .variable
            .as_ref()
            .is_some_and(|v| variables.contains(v))
        {
            self.variable = variables.first().cloned();
        }
        let chosen = self.variable.clone();
        self.covariates
            .retain(|c| variables.contains(c) && Some(c) != chosen.as_ref());
    }

    fn choose_connectome(&mut self, env: &GroupEnv) {
        let kind = self.kind;
        if !kind.needs_connectome() {
            return;
        }
        let Some(entries) = self.connectomes.entries() else {
            return;
        };
        let usable = eligible(kind, entries);
        let current = self.connectome.get(&kind);
        if current.is_some_and(|c| usable.iter().any(|e| &e.spec == c)) {
            return;
        }
        let last = env.last_connectomes.get(kind.record_kind());
        let pick = usable
            .iter()
            .find(|e| Some(&e.spec) == last)
            .or_else(|| usable.first())
            .map(|e| e.spec.clone());
        match pick {
            Some(spec) => {
                self.connectome.insert(kind, spec);
            }
            None => {
                self.connectome.remove(&kind);
            }
        }
    }
}

fn muted(ui: &mut egui::Ui, pal: &Pal, text: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(text).small().color(pal.text_muted))
            .wrap()
            .selectable(false),
    );
}

fn connectome_row(state: &mut ExplorerState, ui: &mut egui::Ui, pal: &Pal) {
    let kind = state.kind;
    let l = ui.label("Connectome");
    ui.horizontal(|ui| {
        let Some(entries) = state.connectomes.entries() else {
            ui.add(egui::Spinner::new());
            return;
        };
        let usable = eligible(kind, entries);
        if usable.is_empty() {
            muted(ui, pal, "None installed; use Connectomes…");
        } else {
            let current = state.connectome.get(&kind).cloned();
            let text = usable
                .iter()
                .find(|e| Some(&e.spec) == current.as_ref())
                .map_or_else(String::new, |e| e.label.clone());
            let mut choice = current.clone();
            egui::ComboBox::from_id_salt("explorer_connectome")
                .selected_text(text)
                .show_ui(ui, |ui| {
                    for e in &usable {
                        ui.selectable_value(&mut choice, Some(e.spec.clone()), e.label.as_str());
                    }
                })
                .response
                .labelled_by(l.id)
                .on_hover_text(match kind {
                    Kind::FibreFiltering => "The structural connectome whose fibres are scored",
                    _ => "The connectome the per-patient connectivity maps are computed with",
                });
            if choice != current {
                if let Some(spec) = choice {
                    state
                        .remembered
                        .replace((kind.record_kind().to_owned(), spec.clone()));
                    state.connectome.insert(kind, spec);
                }
            }
        }
        if ui
            .small_button("Refresh")
            .on_hover_text("Look for installed connectomes again")
            .clicked()
        {
            state.connectomes.reload();
        }
    });
    ui.end_row();
}

fn variable_rows(state: &mut ExplorerState, ui: &mut egui::Ui, variables: &[String]) {
    let l = ui.label("Variable");
    let mut choice = state.variable.clone();
    egui::ComboBox::from_id_salt("explorer_variable")
        .selected_text(choice.clone().unwrap_or_default())
        .show_ui(ui, |ui| {
            for v in variables {
                ui.selectable_value(&mut choice, Some(v.clone()), v.as_str());
            }
        })
        .response
        .labelled_by(l.id)
        .on_hover_text("The clinical outcome the analysis tries to explain");
    ui.end_row();
    if choice != state.variable {
        state.variable = choice;
        let chosen = state.variable.clone();
        state.covariates.retain(|c| Some(c) != chosen.as_ref());
    }
    let l = ui.label("Covariates");
    let text = if state.covariates.is_empty() {
        "None".to_owned()
    } else {
        state.covariates.join(", ")
    };
    egui::ComboBox::from_id_salt("explorer_covariates")
        .selected_text(text)
        .show_ui(ui, |ui| {
            for v in variables
                .iter()
                .filter(|v| Some(*v) != state.variable.as_ref())
            {
                let mut on = state.covariates.contains(v);
                if ui.checkbox(&mut on, v.as_str()).changed() {
                    if on {
                        state.covariates.push(v.clone());
                    } else {
                        state.covariates.retain(|c| c != v);
                    }
                }
            }
        })
        .response
        .labelled_by(l.id)
        .on_hover_text("Variables whose effect is removed from the outcome before the analysis");
    ui.end_row();
}

fn results_ui(
    state: &mut ExplorerState,
    ui: &mut egui::Ui,
    pal: &Pal,
    busy: bool,
    requests: &mut Vec<Request>,
) {
    let kind = state.kind;
    let rows: Vec<ResultRow> = state.results.of_kind(kind).cloned().collect();
    if rows.is_empty() {
        muted(ui, pal, "No results yet");
        return;
    }
    muted(
        ui,
        pal,
        "Results, newest first: id · date · variable · n · r · p · AUC",
    );
    for row in rows {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(&row.id).strong()).selectable(false));
            ui.add(
                egui::Label::new(egui::RichText::new(row_text(&row)).color(pal.text_muted))
                    .selectable(false),
            );
            let asking = state.confirm.as_ref() == Some(&(kind, row.id.clone()));
            if asking {
                if ui
                    .add_enabled(!busy, egui::Button::new("Delete").small())
                    .on_hover_text("Delete the result folder and remove it from the group")
                    .clicked()
                {
                    requests.push(Request::Delete(kind, row.id.clone()));
                    state.confirm = None;
                }
                if ui
                    .small_button("Keep")
                    .on_hover_text("Keep the result")
                    .clicked()
                {
                    state.confirm = None;
                }
                return;
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Show in 3D").small())
                .on_hover_text("Draw this result in the 3D viewer together with the group")
                .clicked()
            {
                requests.push(Request::Show(kind, row.id.clone()));
            }
            if ui
                .small_button("Open folder")
                .on_hover_text("Show the result folder in the file manager")
                .clicked()
            {
                requests.push(Request::OpenFolder(row.folder.clone()));
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Delete").small())
                .on_hover_text("Ask to delete this result")
                .clicked()
            {
                state.confirm = Some((kind, row.id.clone()));
            }
        });
    }
}

pub fn show(state: &mut ExplorerState, ui: &mut egui::Ui, cx: &Cx<'_>, pal: &Pal) -> Vec<Request> {
    let ctx = ui.ctx().clone();
    state
        .connectomes
        .drive(&ctx, cx.env.connectomes_dir.as_deref());
    state.results.drive(&ctx, cx.folder, &cx.record.analyses);
    let variables: Vec<String> = cx.record.clinical.iter().map(|v| v.name.clone()).collect();
    state.normalise(&variables);
    state.choose_connectome(cx.env);
    let mut requests = Vec::new();
    ui.horizontal_wrapped(|ui| {
        for k in Kind::ALL {
            ui.selectable_value(&mut state.kind, k, k.label())
                .on_hover_text(k.tip());
        }
    });
    let kind = state.kind;
    if variables.is_empty() {
        muted(
            ui,
            pal,
            "Import clinical variables in Clinical variables to explain an outcome",
        );
    }
    egui::Grid::new(("explorer_grid", kind.record_kind()))
        .num_columns(2)
        .show(ui, |ui| {
            if kind == Kind::SweetSpot {
                sweet_rows(state, ui);
            }
            variable_rows(state, ui, &variables);
            if kind.needs_connectome() {
                connectome_row(state, ui, pal);
            }
            match kind {
                Kind::SweetSpot => {}
                Kind::FibreFiltering => fibre_rows(state, ui),
                Kind::NetworkMapping => netmap_rows(state, ui),
            }
            cv_rows(ui, &mut state.cv);
        });
    egui::CollapsingHeader::new("Advanced")
        .id_salt(("explorer_advanced", kind.record_kind()))
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new(("explorer_advanced_grid", kind.record_kind()))
                .num_columns(2)
                .show(ui, |ui| match kind {
                    Kind::SweetSpot => sweet_advanced(state, ui),
                    Kind::FibreFiltering => fibre_advanced(state, ui),
                    Kind::NetworkMapping => netmap_advanced(state, ui),
                });
        })
        .header_response
        .on_hover_text("Every other setting of the analysis, with its default");
    let gate = Gate {
        variables: &variables,
        included: cx.included,
        computed: cx.computed,
        busy: cx.busy,
        connectomes: state.connectomes.entries(),
    };
    let reason = run_block_reason(
        kind,
        state.variable.as_deref(),
        state.connectome.get(&kind).map(String::as_str),
        &gate,
    );
    ui.horizontal(|ui| {
        if ui
            .add_enabled(reason.is_none(), egui::Button::new("Run"))
            .on_hover_text(reason.unwrap_or(
                "Run the analysis on the included patients with computed VTAs; the result is listed below",
            ))
            .clicked()
        {
            requests.push(Request::Run);
        }
        if let Some(r) = reason {
            muted(ui, pal, r);
        }
    });
    results_ui(state, ui, pal, cx.busy, &mut requests);
    requests
}

#[cfg(test)]
#[path = "group_explorers_tests.rs"]
mod tests;
