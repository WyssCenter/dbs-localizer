// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The *Import images* wizard: loose NIfTI files and DICOM folders become a subject.
//!
//! All rules live in [`dbsloc_pipeline::import`] (`specs/01` §8.4); this module only collects
//! the inputs, runs the slow work on worker threads and reports the outcome.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use dbsloc_pipeline::import::{
    self, Acq, BatchProgress, ConvertedSeries, ImportBatch, ImportError, ImportItem, ImportPlan,
    ImportReport, ImportResult, LookupTable, SeriesInfo, Session, SubjectImport,
};
use dbsloc_ui::PostopModality;
use dbsloc_viz::VolumePreview;
use eframe::egui;

use crate::guided::{centered_column, primary_button};
use crate::panels::{em, palette};

const COLUMN_EM: f32 = 72.0;
const COMBO_EM: f32 = 6.2;
const TICK_EM: f32 = 1.4;
const NUMBER_EM: f32 = 2.0;
const TIME_EM: f32 = 3.2;
const MODALITY_EM: f32 = 2.6;
const VOXEL_EM: f32 = 8.0;
const REMOVE_EM: f32 = 1.8;
const VIEW_EM: f32 = 2.8;
const TABLE_ROWS: f32 = 12.0;
const REPAINT: Duration = Duration::from_millis(250);
const RULES_SUBJECT: &str = "check";

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Images,
    Subject,
    Review,
}

impl Step {
    fn title(self) -> &'static str {
        match self {
            Step::Images => "Images",
            Step::Subject => "Subject and destination",
            Step::Review => "Review",
        }
    }

    fn number(self) -> usize {
        match self {
            Step::Images => 1,
            Step::Subject => 2,
            Step::Review => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportWizardAction {
    None,
    Close,
    Imported {
        root: String,
        subject_folder: String,
        invalidated: Option<String>,
        postop_modality: Option<PostopModality>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Dicom,
    Nifti,
}

#[derive(Clone, Debug)]
pub struct Input {
    pub path: PathBuf,
    pub kind: InputKind,
    pub subject: String,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub item: ImportItem,
    pub include: bool,
    pub info: SeriesInfo,
    pub label: String,
    pub input: usize,
}

#[derive(Clone, Debug)]
pub struct SubjectPlan {
    pub subject: String,
    pub plan: Result<ImportPlan, ImportError>,
}

#[derive(Clone, Debug)]
pub struct SubjectOutcome {
    pub subject: String,
    pub result: Result<ImportReport, ImportError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportGate {
    Blocked,
    ConfirmNoPostop,
    Ready,
}

struct SubjectGroup {
    id: String,
    rows: Vec<usize>,
    dicom_dirs: Vec<PathBuf>,
}

enum PickResult {
    Destination(Option<PathBuf>),
    Nifti(Vec<PathBuf>),
    Folders(Vec<PathBuf>),
}

enum Msg<T> {
    Progress(BatchProgress),
    Done(T),
}

struct ConversionJob {
    rx: mpsc::Receiver<Msg<ImportResult<Vec<ConvertedSeries>>>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    dir: Option<PathBuf>,
    input: PathBuf,
}

impl Drop for ConversionJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

struct ImportJob {
    rx: mpsc::Receiver<Msg<Vec<ImportResult<ImportReport>>>>,
    cancel: Arc<AtomicBool>,
    progress: BatchProgress,
    subjects: Vec<String>,
    batch: bool,
}

impl Drop for ImportJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

type SubjectKey = (String, Vec<ImportItem>, Vec<PathBuf>);
type PlanKey = (String, Option<PathBuf>, bool, bool, Vec<SubjectKey>);

pub struct ImportWizard {
    pub step: Step,
    pub root: String,
    pub subject: String,
    subject_edited: bool,
    pub inputs: Vec<Input>,
    pub rows: Vec<Row>,
    pub per_folder: bool,
    pub keep_sourcedata: bool,
    pub replace_existing: bool,
    pub postop_modality: PostopModality,
    pub plans: Vec<SubjectPlan>,
    pub rules_problem: Option<String>,
    pub results: Option<Vec<SubjectOutcome>>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub temp_dirs: Vec<PathBuf>,
    pub output_dirs: Vec<crate::app::DatasetOutputDir>,
    pub no_postop_dialog: bool,
    pub run_block: Option<&'static str>,
    table: LookupTable,
    table_warning: Option<String>,
    has_converter: bool,
    plan_key: Option<PlanKey>,
    rules_key: Option<Vec<Vec<ImportItem>>>,
    dicom_queue: VecDeque<PathBuf>,
    queued_total: usize,
    queued_done: usize,
    conversion: Option<ConversionJob>,
    import_job: Option<ImportJob>,
    pending_pick: Option<mpsc::Receiver<PickResult>>,
    preview: Option<(usize, VolumePreview)>,
    outcome: Option<ImportWizardAction>,
    import_end: Option<ImportEnd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEnd {
    pub clean: bool,
    pub problem: Option<String>,
}

fn rollback_incomplete(e: &ImportError) -> bool {
    e.message.contains("rollback incomplete")
}

pub(crate) fn import_end(results: Option<&[ImportResult<ImportReport>]>) -> ImportEnd {
    let Some(results) = results else {
        return ImportEnd {
            clean: false,
            problem: Some("the import stopped unexpectedly".to_owned()),
        };
    };
    let problem = results.iter().find_map(|r| match r {
        Err(e) if e.code != import::E_IMPORT_CANCELLED || rollback_incomplete(e) => {
            Some(error_text(e))
        }
        _ => None,
    });
    ImportEnd {
        clean: problem.is_none(),
        problem,
    }
}

impl Drop for ImportWizard {
    fn drop(&mut self) {
        self.remove_temp_dirs();
    }
}

fn is_nifti_name(path: &Path) -> bool {
    let lower = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    lower.ends_with(".nii") || lower.ends_with(".nii.gz")
}

fn nifti_sidecar(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let lower = name.to_lowercase();
    let stem_len = if lower.ends_with(".nii.gz") {
        name.len().checked_sub(7)?
    } else if lower.ends_with(".nii") {
        name.len().checked_sub(4)?
    } else {
        return None;
    };
    let sidecar = path.with_file_name(format!("{}.json", name.get(..stem_len)?));
    sidecar.is_file().then_some(sidecar)
}

fn folder_niftis(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_nifti_name(p))
        .collect();
    out.sort();
    out
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn subject_from_path(dir: &Path) -> (String, Option<String>) {
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut name = file_name_of(&canon);
    if name.eq_ignore_ascii_case("dicom") {
        if let Some(parent) = canon.parent() {
            name = file_name_of(parent);
        }
    }
    import::subject_id_from_folder(&name)
}

fn has_postop_on_disk(root: &Path, folder: &str) -> bool {
    let anat = root
        .join("rawdata")
        .join(folder)
        .join("ses-postop")
        .join("anat");
    std::fs::read_dir(anat)
        .map(|rd| rd.flatten().any(|e| is_nifti_name(&e.path())))
        .unwrap_or(false)
}

fn missing_postop(plan: &ImportPlan) -> bool {
    !plan.items.iter().any(|i| i.session == Session::Postop)
        && !has_postop_on_disk(&plan.root, &plan.subject_folder())
}

fn voxel_text(v: [f32; 3]) -> String {
    if v.iter().all(|x| *x == 0.0) {
        return "—".to_owned();
    }
    format!("{:.2}×{:.2}×{:.2}", v[0], v[1], v[2])
}

fn time_text(info: &SeriesInfo) -> String {
    info.acquisition_datetime
        .as_deref()
        .and_then(|d| d.split_once('T'))
        .map(|(_, t)| t.chars().take(5).collect())
        .unwrap_or_else(|| "—".to_owned())
}

fn ms(seconds: f64) -> String {
    format!("{:.1} ms", seconds * 1000.0)
}

fn row_tip(row: &Row) -> String {
    let i = &row.info;
    let mut lines = vec![row.item.source.display().to_string()];
    if !i.protocol.is_empty() {
        lines.push(format!("Protocol: {}", i.protocol));
    }
    if let Some(d) = &i.acquisition_datetime {
        lines.push(format!("Acquired: {}", d.replace('T', " ")));
    }
    if i.dims != [0; 3] {
        lines.push(format!(
            "{}×{}×{} voxels of {} mm",
            i.dims[0],
            i.dims[1],
            i.dims[2],
            voxel_text(i.voxel_mm)
        ));
    }
    if let Some(t) = i.slice_thickness {
        lines.push(format!("Slice thickness: {t:.2} mm"));
    }
    let mut scanner = Vec::new();
    if let Some(m) = &i.manufacturer {
        scanner.push(m.clone());
    }
    if let Some(f) = i.magnetic_field_strength {
        scanner.push(format!("{f:.1} T"));
    }
    if !scanner.is_empty() {
        lines.push(format!("Scanner: {}", scanner.join(", ")));
    }
    let times: Vec<String> = [
        ("TE", i.echo_time),
        ("TR", i.repetition_time),
        ("TI", i.inversion_time),
    ]
    .into_iter()
    .filter_map(|(n, v)| v.map(|v| format!("{n} {}", ms(v))))
    .collect();
    if !times.is_empty() {
        lines.push(times.join(", "));
    }
    if !i.image_type.is_empty() {
        lines.push(format!("Image type: {}", i.image_type.join("\\")));
    }
    lines.join("\n")
}

fn as_series(row: &Row) -> ConvertedSeries {
    ConvertedSeries {
        nifti: row.item.source.clone(),
        sidecar: row.item.sidecar.clone(),
        series_number: row.info.series_number,
        description: row.info.description.clone(),
        protocol: row.info.protocol.clone(),
        modality: row.info.modality.clone(),
        dims: row.info.dims,
        voxel_mm: row.info.voxel_mm,
        info: row.info.clone(),
        suggested: row.item.clone(),
        assigned: row.include,
    }
}

fn series_label(description: &str, path: &Path) -> String {
    if description.trim().is_empty() {
        file_name_of(path)
    } else {
        description.to_owned()
    }
}

fn is_postop_mr(item: &ImportItem) -> bool {
    item.session == Session::Postop && item.suffix != "CT"
}

fn normalize_item(item: &mut ImportItem) {
    let choices = import::suffix_choices(item.session);
    if !choices.contains(&item.suffix.as_str()) {
        if let Some(first) = choices.first() {
            item.suffix = (*first).to_owned();
        }
    }
    if item.suffix == "CT" {
        item.acq = None;
    } else if is_postop_mr(item) && !matches!(item.acq, Some(Acq::Ax | Acq::Cor | Acq::Sag)) {
        item.acq = Some(Acq::Ax);
    }
}

fn set_session(item: &mut ImportItem, session: Session) {
    item.session = session;
    normalize_item(item);
}

fn error_text(e: &ImportError) -> String {
    format!("{}\n{}", e.message, e.code)
}

fn valid_subject(id: &str) -> bool {
    !id.is_empty() && dbsloc_pipeline::bids::parse_subj_id(&format!("sub-{id}")).is_ok()
}

pub(crate) fn pipeline_postop_modality(m: PostopModality) -> dbsloc_pipeline::bids::PostopModality {
    match m {
        PostopModality::Auto => dbsloc_pipeline::bids::PostopModality::Auto,
        PostopModality::Ct => dbsloc_pipeline::bids::PostopModality::Ct,
        PostopModality::Mri => dbsloc_pipeline::bids::PostopModality::Mri,
    }
}

impl ImportWizard {
    pub fn new(root: &str) -> Self {
        let (table, table_warning) = match LookupTable::from_env() {
            Ok(t) => (t, None),
            Err(e) => (
                import::default_lookup_table(),
                Some(format!(
                    "{} ({}); the built-in suggestion rules are used",
                    e.message,
                    import::LOOKUP_ENV
                )),
            ),
        };
        let mut wizard = ImportWizard {
            step: Step::Images,
            root: root.trim().to_owned(),
            subject: String::new(),
            subject_edited: false,
            inputs: Vec::new(),
            rows: Vec::new(),
            per_folder: true,
            keep_sourcedata: true,
            replace_existing: false,
            postop_modality: PostopModality::Auto,
            plans: Vec::new(),
            rules_problem: None,
            results: None,
            error: None,
            notice: None,
            temp_dirs: Vec::new(),
            output_dirs: Vec::new(),
            no_postop_dialog: false,
            run_block: None,
            table,
            table_warning,
            has_converter: import::find_dcm2niix().is_some(),
            plan_key: None,
            rules_key: None,
            dicom_queue: VecDeque::new(),
            queued_total: 0,
            queued_done: 0,
            conversion: None,
            import_job: None,
            pending_pick: None,
            preview: None,
            outcome: None,
            import_end: None,
        };
        wizard.screenshot_setup();
        wizard
    }

    fn screenshot_setup(&mut self) {
        let action = std::env::var("DBSLOC_APP_SCREENSHOT_ACTION").unwrap_or_default();
        if !action.trim().eq_ignore_ascii_case("import") {
            return;
        }
        if let Some(raw) = std::env::var_os("DBSLOC_APP_IMPORT_INPUTS") {
            let mut files = Vec::new();
            for p in std::env::split_paths(&raw) {
                if p.is_dir() {
                    files.extend(folder_niftis(&p));
                } else if p.is_file() {
                    files.push(p);
                }
            }
            self.add_nifti_files(&files);
        }
        if let Some(root) = std::env::var_os("DBSLOC_APP_IMPORT_ROOT") {
            self.root = root.to_string_lossy().into_owned();
        }
        if std::env::var_os("DBSLOC_APP_IMPORT_LATEST_STUDY").is_some() {
            self.apply_latest_study();
        }
        match std::env::var("DBSLOC_APP_IMPORT_STEP")
            .unwrap_or_default()
            .trim()
        {
            "subject" => self.step = Step::Subject,
            "review" => {
                self.step = Step::Review;
                self.refresh_plan(true);
            }
            _ => {}
        }
    }

    pub fn busy(&self) -> bool {
        self.conversion.is_some() || self.import_job.is_some()
    }

    pub fn importing(&self) -> bool {
        self.import_job.is_some()
    }

    fn refused_by_run(&mut self) -> bool {
        match self.run_block {
            Some(reason) => {
                self.no_postop_dialog = false;
                self.error = Some(reason.to_owned());
                true
            }
            None => false,
        }
    }

    fn subject_id(&self) -> Result<String, String> {
        let s = self.subject.trim();
        let folder = if s.starts_with("sub-") {
            s.to_owned()
        } else {
            format!("sub-{s}")
        };
        dbsloc_pipeline::bids::parse_subj_id(&folder)
            .map_err(|e| format!("{} ({})", e.message, e.code))
    }

    fn subject_folder_exists(&self, id: &str) -> bool {
        let root = self.root.trim();
        !root.is_empty()
            && valid_subject(id)
            && Path::new(root)
                .join("rawdata")
                .join(format!("sub-{id}"))
                .is_dir()
    }

    fn any_subject_exists(&self) -> bool {
        if self.batch_mode() {
            self.groups()
                .iter()
                .any(|g| self.subject_folder_exists(&g.id))
        } else {
            self.subject_id()
                .is_ok_and(|id| self.subject_folder_exists(&id))
        }
    }

    fn output_dir(&self) -> Option<PathBuf> {
        crate::app::output_dir_in(&self.output_dirs, self.root.trim())
    }

    pub fn included_count(&self) -> usize {
        self.rows.iter().filter(|r| r.include).count()
    }

    fn input_rows(&self, input: usize) -> impl Iterator<Item = (usize, &Row)> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter(move |(_, r)| r.input == input)
    }

    pub fn distinct_subjects(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (k, input) in self.inputs.iter().enumerate() {
            if input.subject.is_empty() || self.input_rows(k).next().is_none() {
                continue;
            }
            if !out.contains(&input.subject) {
                out.push(input.subject.clone());
            }
        }
        out
    }

    pub fn batch_mode(&self) -> bool {
        self.per_folder && self.distinct_subjects().len() > 1
    }

    fn dicom_dir_used(&self, input: usize) -> Option<PathBuf> {
        let i = self.inputs.get(input)?;
        (i.kind == InputKind::Dicom && self.input_rows(input).any(|(_, r)| r.include))
            .then(|| i.path.clone())
    }

    fn groups(&self) -> Vec<SubjectGroup> {
        if !self.batch_mode() {
            return vec![SubjectGroup {
                id: self.subject.trim().to_owned(),
                rows: (0..self.rows.len()).collect(),
                dicom_dirs: (0..self.inputs.len())
                    .filter_map(|k| self.dicom_dir_used(k))
                    .collect(),
            }];
        }
        let mut out: Vec<SubjectGroup> = Vec::new();
        for (k, input) in self.inputs.iter().enumerate() {
            let rows: Vec<usize> = self.input_rows(k).map(|(i, _)| i).collect();
            if rows.is_empty() {
                continue;
            }
            let pos = match out.iter().position(|g| g.id == input.subject) {
                Some(p) => p,
                None => {
                    out.push(SubjectGroup {
                        id: input.subject.clone(),
                        rows: Vec::new(),
                        dicom_dirs: Vec::new(),
                    });
                    out.len() - 1
                }
            };
            let dir = self.dicom_dir_used(k);
            if let Some(g) = out.get_mut(pos) {
                g.rows.extend(rows);
                g.dicom_dirs.extend(dir);
            }
        }
        out
    }

    pub fn subject_imports(&self) -> Vec<SubjectImport> {
        let batch = self.batch_mode();
        self.groups()
            .into_iter()
            .map(|g| {
                let items: Vec<ImportItem> = g
                    .rows
                    .iter()
                    .filter_map(|&i| self.rows.get(i))
                    .filter(|r| r.include)
                    .map(|r| r.item.clone())
                    .collect();
                let mut s = SubjectImport::new(g.id, items);
                s.replace_existing = self.replace_existing;
                s.keep_sourcedata = self.keep_sourcedata;
                s.sourcedata_dirs = g.dicom_dirs;
                s
            })
            .filter(|s| !batch || !s.items.is_empty())
            .collect()
    }

    pub fn check_rules(&mut self, force: bool) {
        let subjects = self.subject_imports();
        let key: Vec<Vec<ImportItem>> = subjects.iter().map(|s| s.items.clone()).collect();
        if !force && self.rules_key.as_ref() == Some(&key) {
            return;
        }
        let nowhere =
            std::env::temp_dir().join(format!("dbsloc-import-rules-{}-none", std::process::id()));
        self.rules_problem = subjects
            .iter()
            .filter(|s| !s.items.is_empty())
            .find_map(|s| {
                match import::plan_import_with(&nowhere, None, RULES_SUBJECT, &s.items, &[]) {
                    Err(e) if e.code != dbsloc_pipeline::bids::E_NO_ANCHOR => Some(e.message),
                    _ => None,
                }
            });
        self.rules_key = Some(key);
    }

    pub fn refresh_plan(&mut self, force: bool) {
        let root = self.root.trim().to_owned();
        if root.is_empty() {
            self.plans.clear();
            self.plan_key = None;
            return;
        }
        let subjects = self.subject_imports();
        let output_dir = self.output_dir();
        let batch = self.batch_mode();
        let key: PlanKey = (
            root.clone(),
            output_dir.clone(),
            batch,
            self.keep_sourcedata,
            subjects
                .iter()
                .map(|s| {
                    (
                        s.subject_id.clone(),
                        s.items.clone(),
                        s.sourcedata_dirs.clone(),
                    )
                })
                .collect(),
        );
        if !force && self.plan_key.as_ref() == Some(&key) {
            return;
        }
        self.plans = if batch {
            let b = ImportBatch {
                root: PathBuf::from(&root),
                output_dir,
                subjects: subjects.clone(),
            };
            import::plan_import_batch(&b)
                .into_iter()
                .zip(subjects.iter())
                .map(|(plan, s)| SubjectPlan {
                    subject: s.subject_id.clone(),
                    plan,
                })
                .collect()
        } else {
            subjects
                .iter()
                .map(|s| {
                    let dirs: &[PathBuf] = if s.keep_sourcedata {
                        &s.sourcedata_dirs
                    } else {
                        &[]
                    };
                    SubjectPlan {
                        subject: s.subject_id.clone(),
                        plan: import::plan_import_with(
                            Path::new(&root),
                            output_dir.as_deref(),
                            &s.subject_id,
                            &s.items,
                            dirs,
                        ),
                    }
                })
                .collect()
        };
        self.plan_key = Some(key);
    }

    fn ok_plans(&self) -> impl Iterator<Item = &ImportPlan> + '_ {
        self.plans.iter().filter_map(|p| p.plan.as_ref().ok())
    }

    fn conflict_count(&self) -> usize {
        self.ok_plans().map(|p| p.conflicts().len()).sum()
    }

    fn postop_kinds(&self) -> (bool, bool) {
        let included = self.rows.iter().filter(|r| r.include);
        let mut ct = false;
        let mut mr = false;
        for r in included {
            if r.item.session == Session::Postop {
                if r.item.suffix == "CT" {
                    ct = true;
                } else {
                    mr = true;
                }
            }
        }
        (ct, mr)
    }

    fn postop_choice(&self) -> Option<PostopModality> {
        let (ct, mr) = self.postop_kinds();
        (ct && mr).then_some(self.postop_modality)
    }

    pub fn subject_ok(&self) -> bool {
        if self.root.trim().is_empty() {
            return false;
        }
        if self.batch_mode() {
            let groups = self.groups();
            !groups.is_empty() && groups.iter().all(|g| valid_subject(&g.id))
        } else {
            self.subject_id().is_ok()
        }
    }

    pub fn can_advance(&self) -> bool {
        if self.busy() {
            return false;
        }
        match self.step {
            Step::Images => self.included_count() > 0 && self.rules_problem.is_none(),
            Step::Subject => self.subject_ok(),
            Step::Review => false,
        }
    }

    pub fn import_gate(&self) -> ImportGate {
        if self.step != Step::Review || self.busy() || self.results.is_some() {
            return ImportGate::Blocked;
        }
        let ok: Vec<&ImportPlan> = self.ok_plans().collect();
        if ok.is_empty() || (!self.batch_mode() && ok.len() != self.plans.len()) {
            return ImportGate::Blocked;
        }
        if self.conflict_count() > 0 && !self.replace_existing {
            return ImportGate::Blocked;
        }
        if ok.iter().any(|p| missing_postop(p)) {
            return ImportGate::ConfirmNoPostop;
        }
        ImportGate::Ready
    }

    pub fn can_import(&self) -> bool {
        self.import_gate() != ImportGate::Blocked
    }

    pub fn request_import(&mut self, ctx: Option<egui::Context>) {
        if self.import_gate() != ImportGate::Blocked && self.refused_by_run() {
            return;
        }
        match self.import_gate() {
            ImportGate::Blocked => {}
            ImportGate::ConfirmNoPostop => self.no_postop_dialog = true,
            ImportGate::Ready => self.start_import(ctx),
        }
    }

    pub fn confirm_import(&mut self, ctx: Option<egui::Context>) {
        self.no_postop_dialog = false;
        if self.import_gate() != ImportGate::Blocked {
            self.start_import(ctx);
        }
    }

    fn advance(&mut self) {
        self.notice = None;
        match self.step {
            Step::Images => {
                if !self.subject_edited {
                    self.prefill_subject();
                }
                self.step = Step::Subject;
            }
            Step::Subject => {
                self.refresh_plan(true);
                self.step = Step::Review;
            }
            Step::Review => {}
        }
    }

    fn back(&mut self) {
        match self.step {
            Step::Images => {}
            Step::Subject => self.step = Step::Images,
            Step::Review => self.step = Step::Subject,
        }
    }

    fn input_index(&mut self, path: PathBuf, kind: InputKind) -> usize {
        if let Some(i) = self
            .inputs
            .iter()
            .position(|i| i.path == path && i.kind == kind)
        {
            return i;
        }
        let (subject, warning) = subject_from_path(&path);
        self.inputs.push(Input {
            path,
            kind,
            subject,
            warning,
        });
        self.inputs.len() - 1
    }

    fn prefill_subject(&mut self) {
        let first = self
            .inputs
            .iter()
            .enumerate()
            .find(|(k, i)| !i.subject.is_empty() && self.input_rows(*k).next().is_some())
            .map(|(_, i)| i.subject.clone());
        if let Some(id) = first {
            self.subject = id;
        }
    }

    fn inputs_changed(&mut self) {
        if !self.subject_edited {
            self.prefill_subject();
        }
        self.check_rules(false);
    }

    pub fn add_nifti_files(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if self.rows.iter().any(|r| r.item.source == *path) {
                continue;
            }
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let input = self.input_index(parent, InputKind::Nifti);
            let sidecar = nifti_sidecar(path);
            let info = import::series_info(path, sidecar.as_deref());
            let suggestion = import::suggest(path, sidecar.as_deref(), &self.table);
            let include = suggestion.is_some();
            let item = suggestion.unwrap_or_else(|| import::draft_item(path, sidecar.as_deref()));
            self.rows.push(Row {
                label: series_label(&info.description, path),
                item,
                include,
                info,
                input,
            });
        }
        self.inputs_changed();
    }

    fn add_series(&mut self, input: PathBuf, series: Vec<ConvertedSeries>) {
        let input = self.input_index(input, InputKind::Dicom);
        for s in series {
            self.rows.push(Row {
                label: series_label(&s.description, &s.nifti),
                include: s.assigned,
                item: s.suggested,
                info: s.info,
                input,
            });
        }
        self.inputs_changed();
    }

    pub fn add_folders(&mut self, folders: &[PathBuf], ctx: &egui::Context) {
        for folder in folders {
            if self.inputs.iter().any(|i| i.path == *folder)
                || self.dicom_queue.contains(folder)
                || self.conversion.as_ref().is_some_and(|c| c.input == *folder)
            {
                continue;
            }
            let niftis = folder_niftis(folder);
            if niftis.is_empty() {
                self.dicom_queue.push_back(folder.clone());
                self.queued_total += 1;
            } else {
                self.add_nifti_files(&niftis);
            }
        }
        self.start_next_dicom(ctx);
    }

    pub fn remove_row(&mut self, i: usize) {
        if i < self.rows.len() {
            self.rows.remove(i);
            self.shift_preview_after_removal(&[i]);
            self.inputs_changed();
        }
    }

    pub fn remove_input(&mut self, k: usize) {
        if k >= self.inputs.len() {
            return;
        }
        let removed: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.input == k)
            .map(|(i, _)| i)
            .collect();
        self.rows.retain(|r| r.input != k);
        self.shift_preview_after_removal(&removed);
        for r in &mut self.rows {
            if r.input > k {
                r.input -= 1;
            }
        }
        self.inputs.remove(k);
        self.inputs_changed();
    }

    pub fn apply_latest_study(&mut self) -> usize {
        let groups = self.groups();
        let mut changed = 0;
        for g in groups {
            let mut series: Vec<ConvertedSeries> = g
                .rows
                .iter()
                .filter_map(|&i| self.rows.get(i))
                .map(as_series)
                .collect();
            for k in import::apply_latest_study_rule(&mut series) {
                let (Some(&i), Some(s)) = (g.rows.get(k), series.get(k)) else {
                    continue;
                };
                if let Some(r) = self.rows.get_mut(i) {
                    r.item = s.suggested.clone();
                    changed += 1;
                }
            }
        }
        self.notice = Some(match changed {
            0 => "No series changed: this needs two or more study dates and pre-op MR series in \
                  the latest one"
                .to_owned(),
            1 => "1 series of the latest study set to post-op".to_owned(),
            n => format!("{n} series of the latest study set to post-op"),
        });
        self.check_rules(false);
        changed
    }

    pub fn study_groups(&self) -> Vec<(String, Vec<usize>)> {
        let parts: Vec<(Option<String>, Vec<usize>)> = if self.batch_mode() {
            self.groups()
                .into_iter()
                .map(|g| (Some(format!("sub-{}", g.id)), g.rows))
                .collect()
        } else {
            vec![(None, (0..self.rows.len()).collect())]
        };
        let mut out = Vec::new();
        for (prefix, rows) in parts {
            let series: Vec<ConvertedSeries> = rows
                .iter()
                .filter_map(|&i| self.rows.get(i))
                .map(as_series)
                .collect();
            for study in import::group_studies(&series) {
                let idx: Vec<usize> = study
                    .series
                    .iter()
                    .filter_map(|&k| rows.get(k).copied())
                    .collect();
                let date = study
                    .date
                    .map_or_else(|| "No study date".to_owned(), |d| format!("Study {d}"));
                let header = match &prefix {
                    Some(p) => format!("{p} · {date}"),
                    None => date,
                };
                out.push((header, idx));
            }
        }
        out
    }

    fn shift_preview_after_removal(&mut self, removed: &[usize]) {
        let Some((row, _)) = &self.preview else {
            return;
        };
        if removed.contains(row) {
            self.preview = None;
        } else {
            let before = removed.iter().filter(|&&r| r < *row).count();
            if let Some((row, _)) = &mut self.preview {
                *row -= before;
            }
        }
    }

    pub fn open_preview(&mut self, i: usize, ctx: &egui::Context) {
        let Some(row) = self.rows.get(i) else {
            return;
        };
        let ct_hint = (row.item.suffix == "CT").then_some(true);
        let title = format!("Image preview — {}", row.label);
        self.preview = Some((
            i,
            VolumePreview::open(row.item.source.clone(), title, ct_hint, ctx),
        ));
    }

    #[cfg(test)]
    pub fn preview_row(&self) -> Option<usize> {
        self.preview.as_ref().map(|(i, _)| *i)
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        let Some((_, preview)) = &mut self.preview else {
            return;
        };
        let mut open = true;
        let screen = ctx.content_rect().size();
        egui::Window::new(preview.title().to_owned())
            .id(egui::Id::new("import_image_preview"))
            .open(&mut open)
            .resizable(true)
            .default_size(screen * 0.6)
            .show(ctx, |ui| preview.show(ui));
        if !open {
            self.preview = None;
        }
    }

    fn remove_temp_dirs(&mut self) {
        self.preview = None;
        for dir in self.temp_dirs.drain(..) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    pub fn spawn_conversion<F>(
        &mut self,
        input: PathBuf,
        dir: Option<PathBuf>,
        job: F,
        ctx: Option<egui::Context>,
    ) where
        F: FnOnce(&AtomicBool) -> ImportResult<Vec<ConvertedSeries>> + Send + 'static,
    {
        if self.busy() {
            return;
        }
        self.error = None;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let result = job(worker_cancel.as_ref());
            let _ = tx.send(Msg::Done(result));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.conversion = Some(ConversionJob {
            rx,
            cancel,
            started: Instant::now(),
            dir,
            input,
        });
    }

    pub fn spawn_import<F>(
        &mut self,
        subjects: Vec<String>,
        batch: bool,
        job: F,
        ctx: Option<egui::Context>,
    ) where
        F: FnOnce(&AtomicBool, &mut dyn FnMut(BatchProgress)) -> Vec<ImportResult<ImportReport>>
            + Send
            + 'static,
    {
        if self.busy() {
            return;
        }
        self.error = None;
        self.notice = None;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let tx_progress = tx.clone();
            let ctx_progress = ctx.clone();
            let result = job(worker_cancel.as_ref(), &mut |p| {
                let _ = tx_progress.send(Msg::Progress(p));
                if let Some(c) = &ctx_progress {
                    c.request_repaint();
                }
            });
            let _ = tx.send(Msg::Done(result));
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.import_job = Some(ImportJob {
            rx,
            cancel,
            progress: BatchProgress::default(),
            subjects,
            batch,
        });
    }

    fn start_next_dicom(&mut self, ctx: &egui::Context) {
        while self.conversion.is_none() {
            let Some(folder) = self.dicom_queue.pop_front() else {
                self.queued_total = 0;
                self.queued_done = 0;
                return;
            };
            if !self.start_dicom(folder, ctx) {
                self.dicom_queue.clear();
                self.queued_total = 0;
                self.queued_done = 0;
                return;
            }
        }
    }

    fn start_dicom(&mut self, folder: PathBuf, ctx: &egui::Context) -> bool {
        if import::find_dcm2niix().is_none() {
            self.error = Some(format!(
                "{} holds no NIfTI files, and dcm2niix was not found to convert DICOM: set {} or \
                 install dcm2niix on PATH\n{}",
                folder.display(),
                import::DCM2NIIX_ENV,
                import::E_DCM2NIIX
            ));
            return false;
        }
        let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("dbsloc_import_{}_{n}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Cannot create {}: {e}", dir.display()));
            return false;
        }
        self.temp_dirs.push(dir.clone());
        let out = dir.clone();
        let table = self.table.clone();
        let source = folder.clone();
        self.spawn_conversion(
            folder,
            Some(dir),
            move |cancel| {
                if !import::looks_like_dicom(&source) {
                    return Err(ImportError {
                        code: import::E_DCM2NIIX,
                        message: format!("no DICOM or NIfTI files found in {}", source.display()),
                    });
                }
                import::convert_dicom_with(&source, &out, cancel, &table)
            },
            Some(ctx.clone()),
        );
        true
    }

    fn start_import(&mut self, ctx: Option<egui::Context>) {
        if self.refused_by_run() {
            return;
        }
        let replace = self.replace_existing;
        if self.batch_mode() {
            let subjects: Vec<SubjectImport> = self
                .subject_imports()
                .into_iter()
                .zip(self.plans.iter())
                .filter(|(_, p)| p.plan.is_ok())
                .map(|(s, _)| s)
                .collect();
            let names = subjects.iter().map(|s| s.subject_id.clone()).collect();
            let batch = ImportBatch {
                root: PathBuf::from(self.root.trim()),
                output_dir: self.output_dir(),
                subjects,
            };
            self.spawn_import(
                names,
                true,
                move |cancel, on| import::run_import_batch(&batch, cancel, &mut |p| on(p.clone())),
                ctx,
            );
        } else {
            let Some(Ok(plan)) = self.plans.first().map(|p| p.plan.clone()) else {
                return;
            };
            let sid = plan.subject_id.clone();
            self.spawn_import(
                vec![sid.clone()],
                false,
                move |cancel, on| {
                    vec![import::run_import(&plan, replace, cancel, &mut |file| {
                        on(BatchProgress {
                            subject: 0,
                            subjects: 1,
                            subject_id: sid.clone(),
                            file,
                        })
                    })]
                },
                ctx,
            );
        }
    }

    pub(crate) fn cancel_job(&mut self) {
        self.dicom_queue.clear();
        if let Some(j) = &self.conversion {
            j.cancel.store(true, Ordering::SeqCst);
        }
        if let Some(j) = &self.import_job {
            j.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn open_picker(&mut self, ctx: &egui::Context, kind: PickKind) {
        if self.pending_pick.is_some() {
            return;
        }
        let ctx = ctx.clone();
        let start = self.root.trim().to_owned();
        let (tx, rx) = mpsc::channel();
        self.pending_pick = Some(rx);
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new();
            if !start.is_empty() && kind == PickKind::Destination {
                dialog = dialog.set_directory(&start);
            }
            let result = match kind {
                PickKind::Destination => PickResult::Destination(
                    dialog.set_title("Select the input directory").pick_folder(),
                ),
                PickKind::Nifti => PickResult::Nifti(
                    dialog
                        .set_title("Select NIfTI files")
                        .add_filter("NIfTI", &["nii", "gz"])
                        .pick_files()
                        .unwrap_or_default(),
                ),
                PickKind::Folders => PickResult::Folders(
                    dialog
                        .set_title("Select DICOM or NIfTI folders")
                        .pick_folders()
                        .unwrap_or_default(),
                ),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        self.poll_pick(ctx);
        self.poll_conversion(ctx);
        self.poll_import();
        if self.busy() || self.pending_pick.is_some() {
            ctx.request_repaint_after(REPAINT);
        }
    }

    fn poll_pick(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.pending_pick else { return };
        let pick = match rx.try_recv() {
            Ok(p) => p,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_pick = None;
                return;
            }
        };
        self.pending_pick = None;
        match pick {
            PickResult::Destination(Some(p)) => self.root = p.to_string_lossy().into_owned(),
            PickResult::Nifti(paths) => self.add_nifti_files(&paths),
            PickResult::Folders(folders) => self.add_folders(&folders, ctx),
            PickResult::Destination(None) => {}
        }
    }

    fn poll_conversion(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.conversion else { return };
        let result = match job.rx.try_recv() {
            Ok(Msg::Done(r)) => r,
            Ok(Msg::Progress(_)) | Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(ImportError {
                code: import::E_DCM2NIIX,
                message: "the conversion stopped unexpectedly".to_owned(),
            }),
        };
        let Some(job) = self.conversion.take() else {
            return;
        };
        let dir = job.dir.clone();
        let input = job.input.clone();
        drop(job);
        self.queued_done += 1;
        match result {
            Ok(series) => {
                self.add_series(input, series);
                self.start_next_dicom(ctx);
            }
            Err(e) => {
                if let Some(d) = dir {
                    let _ = std::fs::remove_dir_all(&d);
                    self.temp_dirs.retain(|t| *t != d);
                }
                if e.code == import::E_IMPORT_CANCELLED {
                    self.dicom_queue.clear();
                    self.notice = Some("Conversion cancelled".to_owned());
                    self.start_next_dicom(ctx);
                } else {
                    self.error = Some(format!("{}\n{}", input.display(), error_text(&e)));
                    self.start_next_dicom(ctx);
                }
            }
        }
    }

    fn poll_import(&mut self) {
        let Some(job) = self.import_job.as_mut() else {
            return;
        };
        let mut finished = None;
        let mut lost = false;
        loop {
            match job.rx.try_recv() {
                Ok(Msg::Progress(p)) => job.progress = p,
                Ok(Msg::Done(r)) => {
                    finished = Some(r);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    lost = true;
                    break;
                }
            }
        }
        if finished.is_none() && !lost {
            return;
        }
        let Some(job) = self.import_job.take() else {
            return;
        };
        let subjects = job.subjects.clone();
        let batch = job.batch;
        drop(job);
        self.import_end = Some(import_end(finished.as_deref()));
        let Some(results) = finished else {
            self.error = Some(format!(
                "the import stopped unexpectedly\n{}",
                import::E_IMPORT_FILE
            ));
            self.refresh_plan(true);
            return;
        };
        if !batch {
            match results.into_iter().next() {
                Some(Ok(report)) => {
                    self.remove_temp_dirs();
                    self.outcome = Some(ImportWizardAction::Imported {
                        root: self.root.trim().to_owned(),
                        invalidated: report
                            .invalidation
                            .summary("the import replaced raw images"),
                        subject_folder: report.subject_folder,
                        postop_modality: self.postop_choice(),
                    });
                }
                Some(Err(e))
                    if e.code == import::E_IMPORT_CANCELLED && !rollback_incomplete(&e) =>
                {
                    self.notice = Some("Import cancelled; nothing was written".to_owned());
                    self.refresh_plan(true);
                }
                Some(Err(e)) => {
                    self.error = Some(error_text(&e));
                    self.refresh_plan(true);
                }
                None => self.refresh_plan(true),
            }
            return;
        }
        if let Some(e) = results
            .iter()
            .find_map(|r| r.as_ref().err().filter(|e| rollback_incomplete(e)))
        {
            self.error = Some(error_text(e));
        } else if results
            .iter()
            .any(|r| matches!(r, Err(e) if e.code == import::E_IMPORT_CANCELLED))
        {
            self.notice = Some(
                "Import cancelled; the subject being written was rolled back and the rest skipped"
                    .to_owned(),
            );
        }
        self.results = Some(
            subjects
                .into_iter()
                .zip(results)
                .map(|(subject, result)| SubjectOutcome { subject, result })
                .collect(),
        );
    }

    pub fn batch_outcome(&self) -> Option<ImportWizardAction> {
        let results = self.results.as_ref()?;
        let first = results.iter().find_map(|r| r.result.as_ref().ok())?;
        let invalidated: Vec<String> = results
            .iter()
            .filter_map(|r| r.result.as_ref().ok())
            .filter_map(|rep| {
                rep.invalidation.summary(&format!(
                    "the import replaced raw images of {}",
                    rep.subject_folder
                ))
            })
            .collect();
        Some(ImportWizardAction::Imported {
            root: self.root.trim().to_owned(),
            subject_folder: first.subject_folder.clone(),
            invalidated: (!invalidated.is_empty()).then(|| invalidated.join("; ")),
            postop_modality: self.postop_choice(),
        })
    }

    pub fn take_import_end(&mut self) -> Option<ImportEnd> {
        self.import_end.take()
    }

    pub fn take_outcome(&mut self) -> ImportWizardAction {
        self.outcome.take().unwrap_or(ImportWizardAction::None)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> ImportWizardAction {
        match self.step {
            Step::Images => self.check_rules(false),
            Step::Review if self.results.is_none() && !self.busy() => self.refresh_plan(false),
            _ => {}
        }
        let mut action = ImportWizardAction::None;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if self.error.is_some() {
                self.error = None;
            } else if self.no_postop_dialog {
                self.no_postop_dialog = false;
            } else if !self.busy() {
                action = ImportWizardAction::Close;
            }
        }
        let unit = em(ui);
        let pal = palette(ui);
        egui::Panel::bottom("import_actions")
            .frame(
                egui::Frame::NONE
                    .fill(pal.surface)
                    .inner_margin(egui::Margin::symmetric(unit as i8, (unit * 0.5) as i8)),
            )
            .show_separator_line(true)
            .show(ui, |ui| {
                centered_column(ui, COLUMN_EM, |ui| self.bar(ui, ctx, &mut action));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(unit as i8, unit as i8)))
            .show(ui, |ui| {
                centered_column(ui, COLUMN_EM, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "Import images — {} of 3: {}",
                                self.step.number(),
                                self.step.title()
                            ))
                            .heading()
                            .strong(),
                        )
                        .selectable(false),
                    );
                    ui.add_space(unit * 0.6);
                    match self.step {
                        Step::Images => self.images_step(ui, ctx),
                        Step::Subject => self.subject_step(ui, ctx),
                        Step::Review => self.review_step(ui),
                    }
                });
            });
        self.no_postop_window(ctx);
        self.preview_window(ctx);
        self.error_window(ctx);
        let outcome = self.take_outcome();
        if outcome != ImportWizardAction::None {
            return outcome;
        }
        action
    }

    fn bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, action: &mut ImportWizardAction) {
        let unit = em(ui);
        let size = egui::vec2(unit * 5.5, unit * 1.9);
        ui.horizontal(|ui| {
            let busy = self.busy();
            let (hover, label) = if busy {
                ("Stop the running job", "Cancel")
            } else if self.results.is_some() {
                ("Close the wizard", "Close")
            } else {
                ("Close without importing (Esc)", "Cancel")
            };
            if ui
                .add(egui::Button::new(label).min_size(size))
                .on_hover_text(hover)
                .clicked()
            {
                if busy {
                    self.cancel_job();
                } else {
                    *action = ImportWizardAction::Close;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match self.step {
                    Step::Review if self.results.is_some() => {
                        let outcome = self.batch_outcome();
                        let hover = if outcome.is_some() {
                            "Open the dataset at the first imported subject"
                        } else {
                            "No subject was imported"
                        };
                        if primary_button(ui, "Open subject", outcome.is_some(), hover).clicked() {
                            if let Some(o) = outcome {
                                self.remove_temp_dirs();
                                *action = o;
                            }
                        }
                    }
                    Step::Review => {
                        let ok = self.can_import() && self.run_block.is_none();
                        let hover = if let Some(reason) = self.run_block {
                            reason
                        } else if ok {
                            "Write the images into the dataset"
                        } else if self.conflict_count() > 0 && !self.replace_existing {
                            "Tick Replace existing images first"
                        } else {
                            "Fix the problems listed first"
                        };
                        if primary_button(ui, "Import", ok, hover)
                            .on_disabled_hover_text(hover)
                            .clicked()
                        {
                            self.request_import(Some(ctx.clone()));
                        }
                    }
                    _ => {
                        let ok = self.can_advance();
                        let hover = match (self.step, ok) {
                            (_, true) => "Go to the next step",
                            (Step::Subject, false) => {
                                "Choose an input directory and a valid subject ID"
                            }
                            _ => "Tick at least one image and fix the problems listed",
                        };
                        if primary_button(ui, "Next", ok, hover).clicked() {
                            self.advance();
                        }
                    }
                }
                let back_ok = self.step != Step::Images && !self.busy() && self.results.is_none();
                if ui
                    .add_enabled(back_ok, egui::Button::new("Back").min_size(size))
                    .on_hover_text("Previous step")
                    .clicked()
                {
                    self.back();
                }
            });
        });
    }

    fn images_step(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let unit = em(ui);
        let pal = palette(ui);
        ui.horizontal(|ui| {
            let idle = self.pending_pick.is_none() && !self.busy();
            let folder_hover = if self.has_converter {
                "Add one or several DICOM folders (converted with dcm2niix) or folders of NIfTI files"
            } else {
                "Add folders of NIfTI files; DICOM needs dcm2niix (set DBSLOC_DCM2NIIX_BIN or \
                 install it on PATH)"
            };
            if ui
                .add_enabled(idle, egui::Button::new("Add folders…"))
                .on_hover_text(folder_hover)
                .clicked()
            {
                self.open_picker(ctx, PickKind::Folders);
            }
            if ui
                .add_enabled(idle, egui::Button::new("Add NIfTI files…"))
                .on_hover_text("Add .nii or .nii.gz files; a JSON sidecar beside a file is used")
                .clicked()
            {
                self.open_picker(ctx, PickKind::Nifti);
            }
            if ui
                .add_enabled(
                    idle && !self.rows.is_empty(),
                    egui::Button::new("Post-op = latest study"),
                )
                .on_hover_text(
                    "With two or more study dates, set the ticked pre-op MR series of the latest \
                     study to post-op",
                )
                .clicked()
            {
                self.apply_latest_study();
            }
        });
        if let Some(job) = &self.conversion {
            let total = self.queued_total.max(1);
            let done = self.queued_done.min(total);
            ui.add(
                egui::ProgressBar::new(done as f32 / total as f32)
                    .text(format!(
                        "Converting {} ({} of {}) — {} s",
                        file_name_of(&job.input),
                        done + 1,
                        total,
                        job.started.elapsed().as_secs()
                    ))
                    .desired_width(ui.available_width()),
            )
            .on_hover_text("DICOM folders converted so far; Cancel stops the conversion");
        }
        if let Some(w) = &self.table_warning {
            ui.add(egui::Label::new(egui::RichText::new(w).color(pal.warn)).wrap());
        }
        ui.add_space(unit * 0.4);
        self.inputs_list(ui);
        if let Some(note) = &self.notice {
            ui.label(egui::RichText::new(note).color(pal.info));
        }
        ui.add_space(unit * 0.3);
        self.table(ui);
        ui.add_space(unit * 0.4);
        if let Some(problem) = &self.rules_problem {
            ui.add(egui::Label::new(egui::RichText::new(problem).color(pal.warn)).wrap());
        }
        if !self.rows.is_empty() {
            let left_out = self.rows.len() - self.included_count();
            let text = if left_out == 0 {
                format!("{} images ticked", self.rows.len())
            } else {
                format!(
                    "{} of {} images ticked; {left_out} unticked left out",
                    self.included_count(),
                    self.rows.len()
                )
            };
            ui.label(egui::RichText::new(text).color(pal.text_muted));
        }
    }

    fn inputs_list(&mut self, ui: &mut egui::Ui) {
        if self.inputs.is_empty() {
            return;
        }
        let pal = palette(ui);
        let mut remove = None;
        for (k, input) in self.inputs.iter().enumerate() {
            let count = self.input_rows(k).count();
            ui.horizontal(|ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Remove this folder and its images")
                    .clicked()
                {
                    remove = Some(k);
                }
                let kind = match input.kind {
                    InputKind::Dicom => "DICOM",
                    InputKind::Nifti => "NIfTI",
                };
                ui.label(egui::RichText::new(kind).color(pal.text_muted));
                ui.label(format!(
                    "{} → sub-{} · {count} image{}",
                    file_name_of(&input.path),
                    if input.subject.is_empty() {
                        "?"
                    } else {
                        input.subject.as_str()
                    },
                    if count == 1 { "" } else { "s" }
                ))
                .on_hover_text(input.path.display().to_string());
                if let Some(w) = &input.warning {
                    ui.label(egui::RichText::new("⚠").color(pal.warn))
                        .on_hover_text(w);
                }
            });
        }
        if let Some(k) = remove {
            self.remove_input(k);
        }
        if self.distinct_subjects().len() > 1 {
            ui.checkbox(&mut self.per_folder, "One subject per input folder")
                .on_hover_text(
                    "The folders name different subjects: import each as its own subject, with \
                     the subject ID from its folder name",
                );
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let unit = em(ui);
        let pal = palette(ui);
        let spacing = ui.spacing().item_spacing.x;
        let fixed = (TICK_EM
            + NUMBER_EM
            + TIME_EM
            + MODALITY_EM
            + VOXEL_EM
            + 3.0 * COMBO_EM
            + VIEW_EM
            + REMOVE_EM)
            * unit;
        let label_w = (ui.available_width() - fixed - 11.0 * spacing).max(unit * 8.0);
        let w = Widths {
            tick: unit * TICK_EM,
            number: unit * NUMBER_EM,
            label: label_w,
            time: unit * TIME_EM,
            modality: unit * MODALITY_EM,
            voxel: unit * VOXEL_EM,
            combo: unit * COMBO_EM,
            view: unit * VIEW_EM,
            remove: unit * REMOVE_EM,
            row: unit * 1.8,
        };
        if self.rows.is_empty() {
            ui.label(
                egui::RichText::new("No images yet: add DICOM folders or NIfTI files")
                    .color(pal.text_muted),
            );
            return;
        }
        egui::Grid::new("import_header")
            .num_columns(11)
            .show(ui, |ui| {
                let heads = [
                    ("", w.tick),
                    ("#", w.number),
                    ("Series", w.label),
                    ("Time", w.time),
                    ("Mod.", w.modality),
                    ("Voxel size (mm)", w.voxel),
                    ("Session", w.combo),
                    ("Type", w.combo),
                    ("Orientation", w.combo),
                    ("", w.view),
                    ("", w.remove),
                ];
                for (h, width) in heads {
                    cell(ui, [width, w.row], egui::RichText::new(h).strong());
                }
                ui.end_row();
            });
        let groups = self.study_groups();
        let mut remove = None;
        let mut view = None;
        let mut edited = false;
        egui::ScrollArea::vertical()
            .max_height(w.row * TABLE_ROWS)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (g, (header, idx)) in groups.iter().enumerate() {
                    ui.label(
                        egui::RichText::new(format!("{header} · {} series", idx.len()))
                            .strong()
                            .color(pal.text_muted),
                    );
                    egui::Grid::new(("import_rows", g))
                        .striped(true)
                        .num_columns(11)
                        .show(ui, |ui| {
                            for &i in idx {
                                let Some(row) = self.rows.get_mut(i) else {
                                    continue;
                                };
                                let ev = row_widgets(ui, i, row, &w, pal.text_muted);
                                if ev.remove {
                                    remove = Some(i);
                                }
                                if ev.view {
                                    view = Some(i);
                                }
                                edited |= ev.edited;
                                ui.end_row();
                            }
                        });
                }
            });
        if let Some(i) = remove {
            self.remove_row(i);
        } else if edited {
            self.check_rules(false);
        }
        if let Some(i) = view {
            let ctx = ui.ctx().clone();
            self.open_preview(i, &ctx);
        }
    }

    fn subject_step(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let unit = em(ui);
        let pal = palette(ui);
        if self.batch_mode() {
            ui.label(egui::RichText::new("One subject per input folder").strong());
            let groups = self.groups();
            egui::Grid::new("import_subjects")
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    for g in &groups {
                        let n = g
                            .rows
                            .iter()
                            .filter_map(|&i| self.rows.get(i))
                            .filter(|r| r.include)
                            .count();
                        if valid_subject(&g.id) {
                            ui.label(format!("sub-{}", g.id));
                        } else {
                            ui.label(
                                egui::RichText::new(format!("{:?}: not a valid ID", g.id))
                                    .color(pal.warn),
                            );
                        }
                        ui.label(format!("{n} images"));
                        let state = if self.subject_folder_exists(&g.id) {
                            "adds to the existing subject"
                        } else {
                            "new subject"
                        };
                        ui.label(egui::RichText::new(state).color(pal.text_muted));
                        ui.end_row();
                    }
                });
            for w in self.inputs.iter().filter_map(|i| i.warning.as_ref()) {
                ui.add(egui::Label::new(egui::RichText::new(w).color(pal.text_muted)).wrap());
            }
        } else {
            ui.label("Subject ID");
            let width = ui.available_width().min(unit * 20.0);
            let resp = ui
                .add_sized(
                    [width, unit * 1.6],
                    egui::TextEdit::singleline(&mut self.subject).hint_text("for example 01"),
                )
                .on_hover_text(
                    "Letters and digits; the sub- prefix is optional. Proposed from the first \
                     input folder's name",
                );
            if resp.changed() {
                self.subject_edited = true;
            }
            if !self.subject.trim().is_empty() {
                match self.subject_id() {
                    Ok(id) => {
                        let mut text = format!("will be sub-{id}");
                        if self.subject_folder_exists(&id) {
                            text.push_str(" · adds to the existing subject");
                        }
                        ui.label(egui::RichText::new(text).color(pal.text_muted));
                    }
                    Err(msg) => {
                        ui.label(egui::RichText::new(msg).color(pal.warn));
                    }
                }
            }
            if !self.subject_edited {
                if let Some(w) = self
                    .inputs
                    .iter()
                    .find(|i| i.subject == self.subject.trim())
                    .and_then(|i| i.warning.as_ref())
                {
                    ui.add(egui::Label::new(egui::RichText::new(w).color(pal.text_muted)).wrap());
                }
            }
        }
        ui.add_space(unit * 0.8);
        ui.label("Dataset folder");
        ui.horizontal(|ui| {
            let browse = ui.add_enabled(self.pending_pick.is_none(), egui::Button::new("Browse…"));
            if browse
                .on_hover_text("Choose the folder that holds (or will hold) the dataset")
                .clicked()
            {
                self.open_picker(ctx, PickKind::Destination);
            }
            let width = ui.available_width();
            ui.add_sized(
                [width, unit * 1.6],
                egui::TextEdit::singleline(&mut self.root)
                    .hint_text("Folder that will contain rawdata/")
                    .desired_width(width),
            )
            .on_hover_text("Dataset folder; created when missing");
        });
        if let Some(out) = self.output_dir() {
            ui.label(
                egui::RichText::new(format!("Pipeline output directory: {}", out.display()))
                    .color(pal.text_muted),
            )
            .on_hover_text(
                "Set in the Pipeline panel; holds the Lead-DBS index and the approvals an import \
                 resets",
            );
        }
        ui.add_space(unit * 0.6);
        let has_dicom = (0..self.inputs.len()).any(|k| self.dicom_dir_used(k).is_some());
        if has_dicom {
            ui.checkbox(
                &mut self.keep_sourcedata,
                "Keep a copy of the source DICOM in sourcedata",
            )
            .on_hover_text("Copy each DICOM folder to sourcedata/sub-<id>/DICOM/ in the dataset");
        }
        if self.any_subject_exists() {
            ui.checkbox(&mut self.replace_existing, "Replace existing images")
                .on_hover_text(
                    "Overwrite images the subject already has; restored if the import fails",
                );
        }
        let (ct, mr) = self.postop_kinds();
        if ct && mr {
            ui.add_space(unit * 0.4);
            ui.horizontal(|ui| {
                ui.label("Post-op images for the pipeline");
                egui::ComboBox::from_id_salt("import_postop_modality")
                    .selected_text(self.postop_modality.label())
                    .show_ui(ui, |ui| {
                        for m in PostopModality::ALL {
                            ui.selectable_value(&mut self.postop_modality, m, m.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Both a post-op CT and a post-op MRI are imported: which one localizes \
                         the leads. Automatic uses the CT",
                    );
            });
        }
    }

    fn review_step(&mut self, ui: &mut egui::Ui) {
        let unit = em(ui);
        let pal = palette(ui);
        if let Some(results) = &self.results {
            for r in results {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("sub-{}", r.subject)).strong());
                    match &r.result {
                        Ok(rep) => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "imported {} files",
                                    rep.written.len()
                                ))
                                .color(pal.ok),
                            );
                        }
                        Err(e) if e.code == import::E_IMPORT_CANCELLED => {
                            ui.label(egui::RichText::new("cancelled").color(pal.warn));
                        }
                        Err(e) => {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("failed: {}", e.message))
                                        .color(pal.err),
                                )
                                .wrap(),
                            );
                        }
                    }
                });
            }
            if let Some(note) = &self.notice {
                ui.label(egui::RichText::new(note).color(pal.info));
            }
            return;
        }
        let batch = self.batch_mode();
        egui::ScrollArea::vertical()
            .max_height(unit * 1.6 * TABLE_ROWS)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for sp in &self.plans {
                    if batch {
                        ui.label(egui::RichText::new(format!("sub-{}", sp.subject)).strong());
                    }
                    match &sp.plan {
                        Ok(plan) => plan_lines(ui, plan, &pal),
                        Err(e) => {
                            let text = if batch {
                                format!("{} ({}); this subject is skipped", e.message, e.code)
                            } else {
                                format!("{} ({})", e.message, e.code)
                            };
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).color(pal.warn)).wrap(),
                            );
                        }
                    }
                    ui.add_space(unit * 0.3);
                }
            });
        let left_out = self.rows.len() - self.included_count();
        if left_out > 0 {
            ui.label(
                egui::RichText::new(format!(
                    "{left_out} unticked image{} left out",
                    if left_out == 1 { " is" } else { "s are" }
                ))
                .color(pal.text_muted),
            );
        }
        ui.add_space(unit * 0.5);
        if self.conflict_count() > 0 {
            let busy = self.busy();
            ui.add_enabled(
                !busy,
                egui::Checkbox::new(&mut self.replace_existing, "Replace existing images"),
            )
            .on_hover_text("Overwrite files that already exist; restored if the import fails");
        }
        if let Some(note) = &self.notice {
            ui.label(egui::RichText::new(note).color(pal.info));
        }
        if let Some(job) = &self.import_job {
            let p = &job.progress;
            let subjects = p.subjects.max(1);
            let file_frac = if p.file.total == 0 {
                0.0
            } else {
                p.file.done as f32 / p.file.total as f32
            };
            let frac = (p.subject as f32 + file_frac) / subjects as f32;
            let text = if subjects > 1 {
                format!(
                    "sub-{} ({} of {subjects}) — {} — {}/{}",
                    p.subject_id,
                    p.subject + 1,
                    p.file.current,
                    p.file.done,
                    p.file.total
                )
            } else {
                format!("{} — {}/{}", p.file.current, p.file.done, p.file.total)
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(text)
                    .desired_width(ui.available_width()),
            )
            .on_hover_text("Files written so far");
        }
    }

    fn no_postop_window(&mut self, ctx: &egui::Context) {
        if !self.no_postop_dialog {
            return;
        }
        let names: Vec<String> = self
            .ok_plans()
            .filter(|p| missing_postop(p))
            .map(ImportPlan::subject_folder)
            .collect();
        let mut open = true;
        let mut go = false;
        let mut back = false;
        let run_block = self.run_block;
        egui::Window::new("No post-op image")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "No post-op image is assigned for {}. Import anyway?",
                    names.join(", ")
                ));
                ui.horizontal(|ui| {
                    if primary_button(
                        ui,
                        "Import anyway",
                        run_block.is_none(),
                        "Import without a post-op image; add one later",
                    )
                    .on_disabled_hover_text(run_block.unwrap_or_default())
                    .clicked()
                    {
                        go = true;
                    }
                    if ui
                        .button("Back")
                        .on_hover_text("Return to the review (Esc)")
                        .clicked()
                    {
                        back = true;
                    }
                });
            });
        if go {
            self.confirm_import(Some(ctx.clone()));
        } else if back || !open {
            self.no_postop_dialog = false;
        }
    }

    fn error_window(&mut self, ctx: &egui::Context) {
        let Some(text) = self.error.clone() else {
            return;
        };
        let err = palette_for_ctx(ctx).err;
        let mut open = true;
        let mut dismiss = false;
        egui::Window::new("⚠ Import problem")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(ctx.global_style().spacing.interact_size.y * 30.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&text).monospace().color(err)).wrap());
                ui.horizontal(|ui| {
                    if ui
                        .button("Copy")
                        .on_hover_text("Copy this message to the clipboard")
                        .clicked()
                    {
                        ctx.copy_text(text.clone());
                    }
                    if ui
                        .button("Dismiss")
                        .on_hover_text("Close this message (Esc)")
                        .clicked()
                    {
                        dismiss = true;
                    }
                });
            });
        if !open || dismiss {
            self.error = None;
        }
    }
}

fn plan_lines(ui: &mut egui::Ui, plan: &ImportPlan, pal: &dbsloc_viz::ui_tokens::Palette) {
    for f in &plan.files {
        ui.horizontal(|ui| {
            let source = plan
                .items
                .get(f.item)
                .map(|i| i.source.display().to_string())
                .unwrap_or_default();
            ui.add(egui::Label::new(egui::RichText::new(&f.relative_path).monospace()).truncate())
                .on_hover_text(source);
            if f.exists {
                ui.label(egui::RichText::new("replaces existing").color(pal.warn));
            }
        });
    }
    for a in &plan.sourcedata {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("{} (DICOM copy)", a.relative_path)).monospace(),
            )
            .truncate(),
        )
        .on_hover_text(a.source.display().to_string());
    }
    if missing_postop(plan) {
        ui.label(egui::RichText::new("No post-op image").color(pal.text_muted));
    }
    for w in &plan.warnings {
        ui.add(egui::Label::new(egui::RichText::new(w).color(pal.text_muted)).wrap());
    }
}

fn palette_for_ctx(ctx: &egui::Context) -> dbsloc_viz::ui_tokens::Palette {
    dbsloc_viz::ui_tokens::palette(ctx.global_style().visuals.dark_mode)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickKind {
    Destination,
    Nifti,
    Folders,
}

struct Widths {
    tick: f32,
    number: f32,
    label: f32,
    time: f32,
    modality: f32,
    voxel: f32,
    combo: f32,
    view: f32,
    remove: f32,
    row: f32,
}

#[derive(Default)]
struct RowEvent {
    view: bool,
    remove: bool,
    edited: bool,
}

fn cell(ui: &mut egui::Ui, size: [f32; 2], text: egui::RichText) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(size[0], size[1]),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(size[0], size[1]));
            ui.add(egui::Label::new(text).truncate().selectable(false))
        },
    )
    .inner
}

fn row_widgets(
    ui: &mut egui::Ui,
    i: usize,
    row: &mut Row,
    w: &Widths,
    muted: egui::Color32,
) -> RowEvent {
    let mut ev = RowEvent::default();
    let text = |s: String, include: bool| {
        let t = egui::RichText::new(s);
        if include {
            t
        } else {
            t.color(muted)
        }
    };
    let before = row.include;
    ui.add_sized(
        [w.tick, w.row],
        egui::Checkbox::without_text(&mut row.include),
    )
    .on_hover_text("Import this image; unticked images are left out");
    ev.edited |= before != row.include;
    let inc = row.include;
    let number = row
        .info
        .series_number
        .map_or_else(String::new, |n| n.to_string());
    cell(ui, [w.number, w.row], text(number, inc));
    cell(ui, [w.label, w.row], text(row.label.clone(), inc)).on_hover_text(row_tip(row));
    cell(ui, [w.time, w.row], text(time_text(&row.info), inc)).on_hover_text("Acquisition time");
    let modality = if row.info.modality.is_empty() {
        "—".to_owned()
    } else {
        row.info.modality.clone()
    };
    cell(ui, [w.modality, w.row], text(modality, inc)).on_hover_text("DICOM modality");
    cell(
        ui,
        [w.voxel, w.row],
        text(voxel_text(row.info.voxel_mm), inc),
    )
    .on_hover_text(row_tip(row));

    let mut item = row.item.clone();
    let mut session = item.session;
    egui::ComboBox::from_id_salt(("import_session", i))
        .selected_text(session.label())
        .width(w.combo)
        .show_ui(ui, |ui| {
            for s in Session::ALL {
                ui.selectable_value(&mut session, s, s.label());
            }
        })
        .response
        .on_hover_text("Whether the image was acquired before or after surgery");
    if session != item.session {
        set_session(&mut item, session);
    }

    egui::ComboBox::from_id_salt(("import_suffix", i))
        .selected_text(item.suffix.clone())
        .width(w.combo)
        .show_ui(ui, |ui| {
            for s in import::suffix_choices(item.session) {
                ui.selectable_value(&mut item.suffix, (*s).to_owned(), *s);
            }
        })
        .response
        .on_hover_text("Image type (contrast)");
    normalize_item(&mut item);

    if item.suffix == "CT" {
        cell(ui, [w.combo, w.row], text("—".to_owned(), inc))
            .on_hover_text("CT images carry no orientation");
    } else {
        let postop_mr = is_postop_mr(&item);
        let selected = item.acq.map_or("None", Acq::label);
        egui::ComboBox::from_id_salt(("import_acq", i))
            .selected_text(selected)
            .width(w.combo)
            .show_ui(ui, |ui| {
                if !postop_mr {
                    ui.selectable_value(&mut item.acq, None, "None");
                }
                for a in Acq::ALL {
                    if postop_mr && a == Acq::Iso {
                        continue;
                    }
                    ui.selectable_value(&mut item.acq, Some(a), a.label());
                }
            })
            .response
            .on_hover_text("Slice orientation; post-op MR images need axial, coronal or sagittal");
    }
    if item != row.item {
        row.item = item;
        row.include = true;
        ev.edited = true;
    }

    ev.view = ui
        .add_sized([w.view, w.row], egui::Button::new("View"))
        .on_hover_text("Show this image's slices")
        .clicked();

    ev.remove = ui
        .add_sized([w.remove, w.row], egui::Button::new("×"))
        .on_hover_text("Remove this image from the import")
        .clicked();
    ev
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbsloc_wiz_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn nifti_bytes() -> Vec<u8> {
        let mut b = vec![0u8; 360];
        b[0..4].copy_from_slice(&348i32.to_le_bytes());
        b[40..42].copy_from_slice(&3i16.to_le_bytes());
        for i in 0..3 {
            b[42 + 2 * i..44 + 2 * i].copy_from_slice(&2i16.to_le_bytes());
        }
        for i in 4..8 {
            b[40 + 2 * i..42 + 2 * i].copy_from_slice(&1i16.to_le_bytes());
        }
        b[70..72].copy_from_slice(&2i16.to_le_bytes());
        b[72..74].copy_from_slice(&8i16.to_le_bytes());
        for i in 0..4 {
            b[76 + 4 * i..80 + 4 * i].copy_from_slice(&1.0f32.to_le_bytes());
        }
        b[108..112].copy_from_slice(&352.0f32.to_le_bytes());
        b[252..254].copy_from_slice(&1i16.to_le_bytes());
        b[344..348].copy_from_slice(b"n+1\0");
        b
    }

    pub(super) fn write_nii(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, nifti_bytes()).unwrap();
        p
    }

    fn wizard(root: &Path, subject: &str) -> ImportWizard {
        let mut w = ImportWizard::new(&root.to_string_lossy());
        w.subject = subject.to_owned();
        w.subject_edited = true;
        w
    }

    fn wait(w: &mut ImportWizard, ctx: &egui::Context) {
        let start = Instant::now();
        while w.busy() && start.elapsed() < Duration::from_secs(10) {
            w.poll(ctx);
            thread::sleep(Duration::from_millis(5));
        }
        w.poll(ctx);
    }

    fn series(path: PathBuf, description: &str, n: u32) -> ConvertedSeries {
        ConvertedSeries {
            suggested: import::suggest_item(&path, None),
            nifti: path,
            sidecar: None,
            series_number: Some(n),
            description: description.to_owned(),
            protocol: String::new(),
            modality: "MR".to_owned(),
            dims: [2, 2, 2],
            voxel_mm: [1.0, 1.0, 1.0],
            info: SeriesInfo {
                series_number: Some(n),
                description: description.to_owned(),
                modality: "MR".to_owned(),
                ..Default::default()
            },
            assigned: true,
        }
    }

    fn dated(mut s: ConvertedSeries, date: &str) -> ConvertedSeries {
        s.info.study_date = Some(date.to_owned());
        s.info.acquisition_datetime = Some(format!("{date}T10:30:00"));
        s
    }

    fn convert(w: &mut ImportWizard, input: &Path, list: Vec<ConvertedSeries>) {
        let ctx = egui::Context::default();
        w.spawn_conversion(input.to_path_buf(), None, move |_| Ok(list), None);
        wait(w, &ctx);
    }

    fn to_review(w: &mut ImportWizard) {
        w.step = Step::Subject;
        w.advance();
        assert_eq!(w.step, Step::Review);
    }

    #[test]
    fn preview_follows_its_row() {
        let src = scratch("preview_rows");
        let a = write_nii(&src, "sub-01_T1w.nii");
        let b = write_nii(&src, "sub-01_T2w.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[a, b]);
        assert_eq!(w.rows.len(), 2);
        let ctx = egui::Context::default();
        w.open_preview(1, &ctx);
        assert_eq!(w.preview_row(), Some(1));
        w.remove_row(0);
        assert_eq!(w.preview_row(), Some(0));
        w.remove_row(0);
        assert_eq!(w.preview_row(), None);
    }

    #[test]
    fn preview_closes_when_its_input_or_temp_files_go() {
        let src = scratch("preview_input");
        let a = write_nii(&src, "sub-01_T1w.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[a]);
        let ctx = egui::Context::default();
        w.open_preview(0, &ctx);
        assert!(w.preview_row().is_some());
        let k = w.rows[0].input;
        w.remove_input(k);
        assert_eq!(w.preview_row(), None);
        let b = write_nii(&src, "sub-01_T2w.nii");
        w.add_nifti_files(&[b]);
        w.open_preview(0, &ctx);
        w.remove_temp_dirs();
        assert_eq!(w.preview_row(), None);
    }

    #[test]
    fn images_step_comes_first_and_gates_next() {
        let src = scratch("gate_src");
        let file = write_nii(&src, "sub-01_T1w.nii");
        let mut w = ImportWizard::new("");
        assert_eq!(w.step, Step::Images);
        assert!(!w.can_advance());
        w.add_nifti_files(&[file]);
        assert!(w.can_advance());
        w.rows[0].include = false;
        w.check_rules(false);
        assert!(!w.can_advance());
    }

    #[test]
    fn subject_step_gates_next() {
        let dir = scratch("subj_gate");
        let mut w = ImportWizard::new("");
        w.step = Step::Subject;
        assert!(!w.can_advance());
        w.root = dir.to_string_lossy().into_owned();
        assert!(!w.can_advance());
        w.subject = "bad id!".to_owned();
        assert!(!w.can_advance());
        w.subject = "01".to_owned();
        assert!(w.can_advance());
        w.subject = "sub-01".to_owned();
        assert!(w.can_advance());
        w.root.clear();
        assert!(!w.can_advance());
    }

    #[test]
    fn loose_files_get_suggestions_and_rules_gate_the_images_step() {
        let src = scratch("rows_src");
        let anchor = write_nii(&src, "sub-01_T1w.nii");
        let file = write_nii(&src, "sub-01_T2w.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[anchor, file]);
        assert_eq!(w.rows.len(), 2);
        assert!(w.rows.iter().all(|r| r.include));
        assert_eq!(w.rows[1].item.session, Session::Preop);
        assert_eq!(w.rows[1].item.suffix, "T2w");
        assert!(w.rules_problem.is_none());
        assert!(w.can_advance());

        w.rows[1].item.session = Session::Postop;
        w.rows[1].item.acq = None;
        w.check_rules(false);
        assert!(w.rules_problem.is_some());
        assert!(!w.can_advance());

        w.rows[1].item.acq = Some(Acq::Ax);
        w.check_rules(false);
        assert!(w.rules_problem.is_none());
        assert!(w.can_advance());
    }

    #[test]
    fn unassigned_rows_are_unticked_and_left_out_of_the_plan() {
        let dir = scratch("unassigned");
        let src = scratch("unassigned_src");
        let t1 = write_nii(&src, "sub-01_T1w.nii");
        let other = write_nii(&src, "scan_a.nii");
        let mut w = wizard(&dir, "01");
        w.add_nifti_files(&[t1.clone(), other]);
        assert!(w.rows[0].include);
        assert!(!w.rows[1].include);
        to_review(&mut w);
        let plan = w.plans[0].plan.as_ref().unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].source, t1);

        w.rows[1].item.suffix = "T2w".to_owned();
        w.rows[1].include = true;
        w.refresh_plan(false);
        assert_eq!(w.plans[0].plan.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn editing_an_unticked_row_ticks_it_and_postop_mr_gets_an_orientation() {
        let src = scratch("edit_src");
        let other = write_nii(&src, "scan_b.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[other]);
        assert!(!w.rows[0].include);
        let mut item = w.rows[0].item.clone();
        set_session(&mut item, Session::Postop);
        assert_eq!(item.acq, Some(Acq::Ax));
        item.suffix = "CT".to_owned();
        normalize_item(&mut item);
        assert_eq!(item.acq, None);
    }

    #[test]
    fn injected_conversion_adds_rows_and_cancel_keeps_them() {
        let src = scratch("conv_src");
        let input = scratch("conv_in");
        let a = write_nii(&src, "a_T1w.nii");
        let b = write_nii(&src, "b_T2w.nii");
        let ctx = egui::Context::default();
        let mut w = ImportWizard::new("");
        convert(
            &mut w,
            &input,
            vec![series(a, "MPRAGE", 3), series(b, "T2 TSE", 5)],
        );
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0].label, "MPRAGE");
        assert_eq!(w.rows[0].info.series_number, Some(3));
        assert_eq!(w.rows[0].item.suffix, "T1w");
        assert_eq!(w.rows[1].item.suffix, "T2w");
        assert_eq!(w.inputs.len(), 1);
        assert_eq!(w.inputs[0].kind, InputKind::Dicom);
        assert!(w.can_advance());

        w.spawn_conversion(
            input.clone(),
            None,
            |_| {
                Err(ImportError {
                    code: import::E_IMPORT_CANCELLED,
                    message: "cancelled".to_owned(),
                })
            },
            None,
        );
        wait(&mut w, &ctx);
        assert_eq!(w.rows.len(), 2);
        assert!(w.error.is_none());
        assert!(w.notice.is_some());
    }

    #[test]
    fn unassigned_series_from_dicom_stay_unticked() {
        let src = scratch("unassigned_dcm_src");
        let input = scratch("unassigned_dcm_in");
        let a = write_nii(&src, "a_T1w.nii");
        let b = write_nii(&src, "b_x.nii");
        let mut unknown = series(b, "localizer", 2);
        unknown.assigned = false;
        let mut w = ImportWizard::new("");
        convert(&mut w, &input, vec![series(a, "MPRAGE", 1), unknown]);
        assert!(w.rows[0].include);
        assert!(!w.rows[1].include);
        assert_eq!(w.subject_imports()[0].items.len(), 1);
    }

    #[test]
    fn studies_group_oldest_first_and_latest_study_rule_sets_postop() {
        let src = scratch("study_src");
        let input = scratch("study_in");
        let pre = write_nii(&src, "pre_T1w.nii");
        let post = write_nii(&src, "post_T1w.nii");
        let undated = write_nii(&src, "x_T2w.nii");
        let mut w = ImportWizard::new("");
        convert(
            &mut w,
            &input,
            vec![
                dated(series(post, "T1 post", 7), "2024-06-01"),
                dated(series(pre, "T1 pre", 3), "2024-01-01"),
                series(undated, "T2", 9),
            ],
        );
        let groups = w.study_groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].0, "Study 2024-01-01");
        assert_eq!(groups[0].1, vec![1]);
        assert_eq!(groups[1].0, "Study 2024-06-01");
        assert_eq!(groups[2].0, "No study date");

        assert_eq!(w.apply_latest_study(), 1);
        assert_eq!(w.rows[0].item.session, Session::Postop);
        assert!(matches!(
            w.rows[0].item.acq,
            Some(Acq::Ax | Acq::Cor | Acq::Sag)
        ));
        assert_eq!(w.rows[1].item.session, Session::Preop);
        assert!(w.notice.as_deref().unwrap().contains('1'));
        assert_eq!(w.apply_latest_study(), 0);
    }

    #[test]
    fn subject_id_comes_from_the_first_input_folder() {
        let base = scratch("subj_from");
        let pat = base.join("Pat_01-a");
        std::fs::create_dir_all(&pat).unwrap();
        let file = write_nii(&pat, "t1.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[file]);
        assert_eq!(w.subject, "Pat01a");
        assert!(w.inputs[0].warning.is_some());

        let dicom = base.join("Pat02").join("DICOM");
        std::fs::create_dir_all(&dicom).unwrap();
        assert_eq!(subject_from_path(&dicom).0, "Pat02");

        w.subject = "07".to_owned();
        w.subject_edited = true;
        let other = base.join("Other");
        std::fs::create_dir_all(&other).unwrap();
        let f2 = write_nii(&other, "t2.nii");
        w.add_nifti_files(&[f2]);
        assert_eq!(w.subject, "07");
    }

    #[test]
    fn add_folders_adds_nifti_folders_directly() {
        let base = scratch("folders");
        let pat = base.join("PatX");
        std::fs::create_dir_all(&pat).unwrap();
        write_nii(&pat, "a_T1w.nii");
        write_nii(&pat, "b_T2w.nii.gz");
        let ctx = egui::Context::default();
        let mut w = ImportWizard::new("");
        w.add_folders(std::slice::from_ref(&pat), &ctx);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.inputs.len(), 1);
        assert_eq!(w.subject, "PatX");
        assert!(!w.busy());
    }

    #[test]
    fn no_postop_needs_a_confirmation_not_a_block() {
        let dir = scratch("nopost");
        let src = scratch("nopost_src");
        let t1 = write_nii(&src, "sub-01_T1w.nii");
        let ct = write_nii(&src, "ct.nii");
        let mut w = wizard(&dir, "01");
        w.add_nifti_files(std::slice::from_ref(&t1));
        to_review(&mut w);
        assert_eq!(w.import_gate(), ImportGate::ConfirmNoPostop);
        assert!(w.can_import());
        w.request_import(None);
        assert!(w.no_postop_dialog);
        assert!(!w.busy());

        w.no_postop_dialog = false;
        w.add_nifti_files(&[ct]);
        assert_eq!(w.rows[1].item.suffix, "CT");
        w.refresh_plan(false);
        assert_eq!(w.import_gate(), ImportGate::Ready);
    }

    #[test]
    fn a_run_refuses_the_import_and_keeps_the_choices() {
        let dir = scratch("runblock");
        let src = scratch("runblock_src");
        let t1 = write_nii(&src, "sub-01_T1w.nii");
        let ct = write_nii(&src, "ct.nii");
        let mut w = wizard(&dir, "01");
        w.add_nifti_files(&[t1, ct]);
        to_review(&mut w);
        w.keep_sourcedata = false;
        assert_eq!(w.import_gate(), ImportGate::Ready);

        w.run_block = Some(crate::app::IMPORT_LOCKED_RUN);
        w.request_import(None);
        assert!(!w.busy());
        assert_eq!(w.error.as_deref(), Some(crate::app::IMPORT_LOCKED_RUN));
        assert_eq!(w.step, Step::Review);
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.subject, "01");
        assert!(!w.keep_sourcedata);
        assert!(!dir.join("rawdata").exists());

        w.error = None;
        w.run_block = Some(crate::app::IMPORT_LOCKED_REVIEW);
        w.confirm_import(None);
        assert!(!w.busy());
        assert_eq!(w.error.as_deref(), Some(crate::app::IMPORT_LOCKED_REVIEW));
        assert!(!dir.join("rawdata").exists());

        let only_t1 = scratch("runblock_t1");
        let mut w1 = wizard(&only_t1, "01");
        w1.add_nifti_files(&[write_nii(&src, "sub-01_T1w.nii")]);
        to_review(&mut w1);
        assert_eq!(w1.import_gate(), ImportGate::ConfirmNoPostop);
        w1.run_block = Some(crate::app::IMPORT_LOCKED_BATCH);
        w1.request_import(None);
        assert!(!w1.no_postop_dialog);
        assert!(!w1.busy());
        assert_eq!(w1.error.as_deref(), Some(crate::app::IMPORT_LOCKED_BATCH));

        w.error = None;
        w.run_block = None;
        let ctx = egui::Context::default();
        w.request_import(None);
        assert!(w.importing());
        wait(&mut w, &ctx);
        assert!(w.error.is_none(), "{:?}", w.error);
        assert!(matches!(
            w.take_outcome(),
            ImportWizardAction::Imported { .. }
        ));
    }

    #[test]
    fn no_preop_anchor_blocks_the_import() {
        let dir = scratch("noanchor");
        let src = scratch("noanchor_src");
        let ct = write_nii(&src, "ct.nii");
        let mut w = wizard(&dir, "01");
        w.add_nifti_files(&[ct]);
        assert!(w.can_advance());
        to_review(&mut w);
        let err = w.plans[0].plan.as_ref().unwrap_err();
        assert_eq!(err.code, dbsloc_pipeline::bids::E_NO_ANCHOR);
        assert_eq!(w.import_gate(), ImportGate::Blocked);
    }

    #[test]
    fn keep_sourcedata_reaches_the_plan() {
        let dir = scratch("keep");
        let src = scratch("keep_src");
        let input = scratch("keep_dicom");
        let a = write_nii(&src, "a_T1w.nii");
        let mut w = wizard(&dir, "01");
        convert(&mut w, &input, vec![series(a, "MPRAGE", 1)]);
        to_review(&mut w);
        let plan = w.plans[0].plan.as_ref().unwrap();
        assert_eq!(plan.sourcedata.len(), 1);
        assert!(plan.sourcedata[0]
            .relative_path
            .starts_with("sourcedata/sub-01/DICOM/"));

        w.keep_sourcedata = false;
        w.refresh_plan(false);
        assert!(w.plans[0].plan.as_ref().unwrap().sourcedata.is_empty());

        w.keep_sourcedata = true;
        w.rows[0].include = false;
        assert!(w.subject_imports()[0].sourcedata_dirs.is_empty());
    }

    #[test]
    fn postop_choice_only_when_ct_and_mr_are_both_imported() {
        let src = scratch("choice_src");
        let t1 = write_nii(&src, "sub-01_T1w.nii");
        let ct = write_nii(&src, "ct.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[t1, ct]);
        w.postop_modality = PostopModality::Mri;
        assert_eq!(w.postop_choice(), None);
        let mut mr = w.rows[0].item.clone();
        set_session(&mut mr, Session::Postop);
        w.rows[0].item = mr;
        assert_eq!(w.postop_choice(), Some(PostopModality::Mri));
        assert_eq!(
            pipeline_postop_modality(PostopModality::Ct),
            dbsloc_pipeline::bids::PostopModality::Ct
        );
    }

    #[test]
    fn folders_of_different_subjects_import_one_subject_each() {
        let root = scratch("batch_root");
        let base = scratch("batch_src");
        let ctx = egui::Context::default();
        let mut w = ImportWizard::new(&root.to_string_lossy());
        for name in ["PatA", "PatB"] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            write_nii(&d, "t1.nii");
            write_nii(&d, "ct.nii");
            w.add_folders(&[d], &ctx);
        }
        assert_eq!(w.distinct_subjects(), vec!["PatA", "PatB"]);
        assert!(w.batch_mode());
        assert_eq!(w.study_groups().len(), 2);
        w.advance();
        assert!(w.subject_ok());
        w.advance();
        assert_eq!(w.plans.len(), 2);
        assert!(w.plans.iter().all(|p| p.plan.is_ok()));
        assert_eq!(w.import_gate(), ImportGate::Ready);
        w.request_import(None);
        wait(&mut w, &ctx);
        let results = w.results.as_ref().unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.result.is_ok()));
        assert!(root.join("rawdata/sub-PatA/ses-postop/anat").is_dir());
        assert!(root.join("rawdata/sub-PatB/ses-preop/anat").is_dir());
        match w.batch_outcome().unwrap() {
            ImportWizardAction::Imported { subject_folder, .. } => {
                assert_eq!(subject_folder, "sub-PatA")
            }
            other => panic!("{other:?}"),
        }

        w.per_folder = false;
        assert!(!w.batch_mode());
    }

    #[test]
    fn injected_import_reports_success_and_failure() {
        let dir = scratch("imp");
        let ctx = egui::Context::default();
        let mut w = wizard(&dir, "01");
        let temp = scratch("imp_temp");
        w.temp_dirs.push(temp.clone());
        let root = w.root.clone();
        w.spawn_import(
            vec!["01".to_owned()],
            false,
            |_, on| {
                on(BatchProgress {
                    subject: 0,
                    subjects: 1,
                    subject_id: "01".to_owned(),
                    file: import::ImportProgress {
                        done: 1,
                        total: 1,
                        current: "x".to_owned(),
                    },
                });
                vec![Ok(ImportReport {
                    subject_folder: "sub-01".to_owned(),
                    written: Vec::new(),
                    index_path: PathBuf::new(),
                    invalidation: Default::default(),
                })]
            },
            None,
        );
        wait(&mut w, &ctx);
        assert_eq!(
            w.take_outcome(),
            ImportWizardAction::Imported {
                root,
                subject_folder: "sub-01".to_owned(),
                invalidated: None,
                postop_modality: None,
            }
        );
        assert!(w.temp_dirs.is_empty());
        assert!(!temp.exists());

        w.spawn_import(
            vec!["01".to_owned()],
            false,
            |_, _| {
                vec![Err(ImportError {
                    code: import::E_IMPORT_RULES,
                    message: "broken".to_owned(),
                })]
            },
            None,
        );
        wait(&mut w, &ctx);
        assert!(w.error.as_deref().unwrap().contains("E_IMPORT_RULES"));
        assert_eq!(w.take_outcome(), ImportWizardAction::None);
    }

    fn report() -> ImportResult<ImportReport> {
        Ok(ImportReport {
            subject_folder: "sub-01".to_owned(),
            written: Vec::new(),
            index_path: PathBuf::new(),
            invalidation: Default::default(),
        })
    }

    fn failed(code: &'static str, message: &str) -> ImportResult<ImportReport> {
        Err(ImportError {
            code,
            message: message.to_owned(),
        })
    }

    #[test]
    fn only_a_success_or_a_clean_cancel_ends_the_import_cleanly() {
        let cancelled = || {
            failed(
                import::E_IMPORT_CANCELLED,
                "import cancelled, nothing was kept",
            )
        };
        let incomplete = || {
            failed(
                import::E_IMPORT_CANCELLED,
                "import cancelled, nothing was kept (rollback incomplete: a.nii.gz)",
            )
        };
        let rules = || failed(import::E_IMPORT_RULES, "broken");
        assert!(!import_end(None).clean);
        assert!(import_end(None).problem.is_some());
        assert!(import_end(Some(&[report()])).clean);
        assert!(import_end(Some(&[cancelled()])).clean);
        assert!(import_end(Some(&[report(), cancelled()])).clean);
        let end = import_end(Some(&[incomplete()]));
        assert!(!end.clean);
        assert!(end.problem.unwrap().contains("rollback incomplete"));
        let end = import_end(Some(&[report(), rules()]));
        assert!(!end.clean);
        assert!(end.problem.unwrap().contains("E_IMPORT_RULES"));
    }

    #[test]
    fn an_incomplete_rollback_after_a_cancel_is_shown_as_an_error() {
        let dir = scratch("rollback");
        let ctx = egui::Context::default();
        let mut w = wizard(&dir, "01");
        w.spawn_import(
            vec!["01".to_owned()],
            false,
            |_, _| {
                vec![failed(
                    import::E_IMPORT_CANCELLED,
                    "import cancelled, nothing was kept (rollback incomplete: a.nii.gz)",
                )]
            },
            None,
        );
        wait(&mut w, &ctx);
        assert!(w.notice.is_none());
        assert!(w.error.as_deref().unwrap().contains("rollback incomplete"));
        assert!(!w.take_import_end().unwrap().clean);

        w.error = None;
        w.spawn_import(
            vec!["01".to_owned()],
            false,
            |_, _| {
                vec![failed(
                    import::E_IMPORT_CANCELLED,
                    "import cancelled, nothing was kept",
                )]
            },
            None,
        );
        wait(&mut w, &ctx);
        assert!(w.error.is_none());
        assert!(w.notice.is_some());
        assert!(w.take_import_end().unwrap().clean);
        assert!(w.take_import_end().is_none());
    }

    #[test]
    fn drop_removes_temp_dirs() {
        let temp = scratch("drop_temp");
        let mut w = ImportWizard::new("");
        w.temp_dirs.push(temp.clone());
        drop(w);
        assert!(!temp.exists());
    }

    #[test]
    fn session_change_resets_invalid_suffix() {
        let src = scratch("suffix_src");
        let file = write_nii(&src, "x_T1w.nii");
        let mut w = ImportWizard::new("");
        w.add_nifti_files(&[file]);
        set_session(&mut w.rows[0].item, Session::Postop);
        assert_eq!(w.rows[0].item.suffix, "T1w");
        w.rows[0].item.suffix = "CT".to_owned();
        set_session(&mut w.rows[0].item, Session::Preop);
        assert_eq!(w.rows[0].item.suffix, "T1w");
    }
}
