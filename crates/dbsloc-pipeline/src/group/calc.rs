// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Group *Calculate* (`specs/14` §4): the contact distance pass and the `vat` stage for the group
//! stimulation label of every selected patient, run as a sequential batch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use fastwarp_core::{apply_rowmajor, invert_rowmajor, voxel_spacing_rowmajor};
use fastwarp_matfile::mat5::mat5_bytes;
use serde_json::{json, Value};

use super::ea_stats::{read_ea_stats, stats_path, EaStats, EA_STATS_VARIABLE};
use super::flip::{mirror_label_dir, remove_flipped, FlippedRemoval};
use super::fliplr::{resolve_or_build, FlipBuildError, FlipEnv, FlipPaths};
use super::overlap::vtastats_path;
use super::record::{subject_id, AnalysisRef, GroupRecord, PatientEntry};
use super::{open_group, save_group, GroupError, Note};
use crate::atlas_catalog::bundled_and_external_atlas_sets;
use crate::error::PipelineError;
use crate::nifti_io::load_volume_f64;
use crate::options::RunOptions;
use crate::outputs::{write_replacing, ReplaceWatch};
use crate::protocol::{RunEvent, StageOutcome};
use crate::sha256::{file_sha256_hex, sha256_hex};
use crate::stages::Stage;
use crate::vat::settings::{
    label_dir, load as load_stimulation, save as save_stimulation, ModelKind, Side, Stimulation,
};
use crate::vat::stage::run_vat_with;
use crate::vat::stage::VatParams;
use crate::vat::stats::StatsAtlas;
use crate::version::{software_stamp, utc_now_rfc3339};

/// Message of a subject that was not run because the batch was cancelled.
pub const CANCELLED_MESSAGE: &str = "cancelled";
/// `kind` and `id` of the analysis record that stores the fingerprints of a calculation.
pub const ANALYSIS_KIND: &str = "atlasstats";
pub const ANALYSIS_ID: &str = "calc";
/// Schema id of the analysis record.
pub const ANALYSIS_SCHEMA: &str = "dbsloc-group-analysis/1";

/// How one subject of the batch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalcPhase {
    /// The statistics were computed (or refreshed).
    Computed,
    /// Every output was up to date.
    Kept,
    /// A step failed.
    Failed,
    /// A prerequisite is missing; nothing ran (counts as a failure).
    NotRun,
    /// The batch was cancelled before or during this subject.
    Cancelled,
}

impl CalcPhase {
    /// `computed`, `kept`, `failed`, `not_run` or `cancelled`.
    pub fn as_str(self) -> &'static str {
        match self {
            CalcPhase::Computed => "computed",
            CalcPhase::Kept => "kept",
            CalcPhase::Failed => "failed",
            CalcPhase::NotRun => "not_run",
            CalcPhase::Cancelled => "cancelled",
        }
    }

    /// Whether the subject counts towards exit code 1.
    pub fn is_failure(self) -> bool {
        matches!(self, CalcPhase::Failed | CalcPhase::NotRun)
    }
}

/// What the contact distance pass did for a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceState {
    /// Written (new file, other atlas set, no distances yet or a newer reconstruction).
    Computed,
    /// The file already held current distances.
    Kept,
    /// Not run (switched off, no atlas, file not rewritable).
    Skipped,
}

impl DistanceState {
    fn as_str(self) -> &'static str {
        match self {
            DistanceState::Computed => "computed",
            DistanceState::Kept => "kept",
            DistanceState::Skipped => "skipped",
        }
    }
}

/// One subject's result.
#[derive(Debug, Clone)]
pub struct SubjectOutcome {
    pub subject: String,
    pub phase: CalcPhase,
    pub code: Option<String>,
    pub message: Option<String>,
    pub distances: DistanceState,
    pub backed_up_stats: bool,
    pub mirrored: Vec<PathBuf>,
    pub fingerprint: Option<Value>,
    pub elapsed_ms: u128,
}

impl SubjectOutcome {
    fn new(subject: &str, phase: CalcPhase) -> SubjectOutcome {
        SubjectOutcome {
            subject: subject.to_owned(),
            phase,
            code: None,
            message: None,
            distances: DistanceState::Skipped,
            backed_up_stats: false,
            mirrored: Vec::new(),
            fingerprint: None,
            elapsed_ms: 0,
        }
    }

    fn with_message(mut self, code: Option<&str>, message: impl Into<String>) -> SubjectOutcome {
        self.code = code.map(str::to_owned);
        self.message = Some(message.into());
        self
    }

    /// The outcome as the JSON summary prints it.
    pub fn to_json(&self) -> Value {
        json!({
            "subject": self.subject,
            "status": self.phase.as_str(),
            "code": self.code,
            "message": self.message,
            "contact_distances": self.distances.as_str(),
            "stats_backup": self.backed_up_stats,
            "mirrored": self.mirrored.len(),
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

/// Progress of a group job.
#[derive(Debug, Clone)]
pub enum CalcEvent {
    /// `index` is 0-based.
    SubjectStarted {
        index: usize,
        total: usize,
        subject: String,
    },
    Log {
        subject: String,
        line: String,
    },
    SubjectFinished(SubjectOutcome),
}

/// The result of a group job.
#[derive(Debug, Clone, Default)]
pub struct CalcReport {
    pub guid: String,
    pub label: String,
    pub atlas_set: Option<String>,
    pub outcomes: Vec<SubjectOutcome>,
    pub notes: Vec<Note>,
    pub elapsed_ms: u128,
    pub fingerprint: Option<String>,
    pub analysis_record: Option<PathBuf>,
    pub interrupted: bool,
}

impl CalcReport {
    /// Whether any subject failed or could not run.
    pub fn any_failure(&self) -> bool {
        self.outcomes.iter().any(|o| o.phase.is_failure())
    }

    /// Whether the batch was cancelled from outside.
    pub fn cancelled(&self) -> bool {
        self.interrupted
    }

    /// The process exit code: 130 on cancel, 1 on any failed subject, else 0 (`specs/14` §4.1).
    pub fn exit_code(&self) -> i32 {
        if self.interrupted {
            130
        } else {
            i32::from(self.any_failure())
        }
    }

    /// Number of subjects in a phase.
    pub fn count(&self, phase: CalcPhase) -> usize {
        self.outcomes.iter().filter(|o| o.phase == phase).count()
    }

    /// The JSON summary of the job.
    pub fn to_json(&self) -> Value {
        json!({
            "guid": self.guid,
            "label": self.label,
            "atlas_set": self.atlas_set,
            "subjects": self.outcomes.iter().map(SubjectOutcome::to_json).collect::<Vec<_>>(),
            "totals": {
                "subjects": self.outcomes.len(),
                "computed": self.count(CalcPhase::Computed),
                "kept": self.count(CalcPhase::Kept),
                "failed": self.count(CalcPhase::Failed) + self.count(CalcPhase::NotRun),
                "cancelled": self.count(CalcPhase::Cancelled),
            },
            "fingerprint": self.fingerprint,
            "analysis_record": self.analysis_record,
            "warnings": self.notes.iter().map(|n| json!(n.to_string())).collect::<Vec<_>>(),
            "elapsed_ms": self.elapsed_ms,
        })
    }
}

/// Which flip transform *Calculate* uses for the mirrored volumes (`specs/14` §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FlipTransformChoice {
    /// The cached transform of each template space, built first when it is missing.
    #[default]
    Auto,
    /// A forward composite of an existing transform.
    Path(PathBuf),
    /// Only x is negated, without the non-linear part (a warning is reported).
    PlainFlip,
}

/// Options of a group job.
#[derive(Debug, Clone)]
pub struct CalcOptions {
    /// Template of the subject options (engines, atlas folders, threads); the dataset, subject, output folder, stimulation label and statistics atlas are set per subject.
    pub run: RunOptions,
    /// Tunables of the `vat` stage.
    pub params: VatParams,
    /// Patients to run (ids with or without `sub-`); `None` = every included patient.
    pub subjects: Option<Vec<String>>,
    pub stop_on_first_failure: bool,
    /// Run the contact distance pass (`specs/14` §3.4).
    pub contact_distances: bool,
    /// Write the flipped VTA / E-field files (`specs/14` §4.4).
    pub mirror: bool,
    /// Flip transform of `specs/14` §4.4 used with `mirror`.
    pub flip_transform: FlipTransformChoice,
    /// User data directory that holds the cached flip transforms; `None` = the default one.
    pub flip_cache_root: Option<PathBuf>,
    /// Create a missing `gs_<guid>` settings file from another label of the same subject.
    pub copy_settings_from: Option<String>,
    pub sync_settings: bool,
}

impl CalcOptions {
    /// Defaults: every included patient, continue on error, distances on, no mirroring.
    pub fn new(run: RunOptions) -> CalcOptions {
        CalcOptions {
            run,
            params: VatParams::default(),
            subjects: None,
            stop_on_first_failure: false,
            contact_distances: true,
            mirror: false,
            flip_transform: FlipTransformChoice::Auto,
            flip_cache_root: None,
            copy_settings_from: None,
            sync_settings: false,
        }
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

fn io_error(path: &Path, e: &std::io::Error) -> GroupError {
    GroupError::Io(format!("cannot write {}: {e}", path.display()))
}

fn reco_path(folder: &Path, subject: &str) -> PathBuf {
    folder
        .join("reconstruction")
        .join(format!("{subject}_desc-reconstruction.json"))
}

fn hash_of(path: &Path) -> Value {
    file_sha256_hex(path).map_or(Value::Null, Value::from)
}

/// The files a subject's group statistics depend on, as hashes (`specs/14` §4.5).
pub fn subject_fingerprint(folder: &Path, subject: &str, label: &str) -> Value {
    let sid = subject_id(subject);
    json!({
        "subject": subject,
        "reconstruction_sha256": hash_of(&reco_path(folder, subject)),
        "stimulation_sha256": hash_of(&crate::vat::settings::stimulation_json_path(folder, sid, label)),
        "vtastats_sha256": vtastats_path(folder, sid, label).map_or(Value::Null, |p| hash_of(&p)),
    })
}

/// The fingerprints of every included patient and their combined hash.
pub fn group_fingerprint(record: &GroupRecord) -> (Vec<Value>, String) {
    let label = record.stim_label();
    let inputs: Vec<Value> = record
        .patients
        .iter()
        .filter(|p| p.included)
        .map(|p| match record.patient_folder(p) {
            Some(dir) => subject_fingerprint(&dir, &p.subject, &label),
            None => json!({"subject": p.subject}),
        })
        .collect();
    let body = json!({
        "label": label,
        "atlas_set": record.atlas_set,
        "model": record.stimulation.as_ref().and_then(|s| s.model).map(ModelKind::slug),
        "inputs": inputs,
    });
    let hash = sha256_hex(body.to_string().as_bytes());
    (inputs, hash)
}

fn analysis_relative() -> String {
    format!("{ANALYSIS_KIND}/{ANALYSIS_ID}/analysis.json")
}

/// Whether the stored calculation no longer matches the files on disk (`specs/14` §4.5): `None` when the group has no calculation record or it cannot be read.
pub fn calc_is_stale(record: &GroupRecord, folder: &Path) -> Option<bool> {
    let entry = record
        .analyses
        .iter()
        .find(|a| a.kind == ANALYSIS_KIND && a.id == ANALYSIS_ID)?;
    let text = std::fs::read_to_string(folder.join(&entry.record)).ok()?;
    let stored: Value = serde_json::from_str(&text).ok()?;
    let stored = stored.get("fingerprint")?.as_str()?.to_owned();
    Some(stored != group_fingerprint(record).1)
}

fn write_calc_record(
    record: &mut GroupRecord,
    folder: &Path,
    selection: &[String],
) -> Result<(PathBuf, String), GroupError> {
    let (inputs, fingerprint) = group_fingerprint(record);
    let relative = analysis_relative();
    let path = folder.join(&relative);
    let body = json!({
        "schema": ANALYSIS_SCHEMA,
        "kind": ANALYSIS_KIND,
        "id": ANALYSIS_ID,
        "software": software_stamp(),
        "guid": record.guid,
        "label": record.stim_label(),
        "atlas_set": record.atlas_set,
        "model": record.stimulation.as_ref().and_then(|s| s.model).map(ModelKind::slug),
        "selection": selection,
        "created_at": utc_now_rfc3339(),
        "inputs": inputs,
        "fingerprint": fingerprint,
    });
    let text = serde_json::to_string_pretty(&body)
        .map(|t| t + "\n")
        .map_err(|e| GroupError::Json(e.to_string()))?;
    write_replacing(&path, text.as_bytes()).map_err(|e| io_error(&path, &e))?;
    record
        .analyses
        .retain(|a| !(a.kind == ANALYSIS_KIND && a.id == ANALYSIS_ID));
    record.analyses.push(AnalysisRef {
        kind: ANALYSIS_KIND.to_owned(),
        id: ANALYSIS_ID.to_owned(),
        record: relative,
    });
    Ok((path, fingerprint))
}

fn contact_coords(reco: &Value, side: Side) -> Vec<[f64; 3]> {
    let key = (side.index() + 1).to_string();
    let block = reco.get("reference").or_else(|| reco.get("mni"));
    block
        .and_then(|b| b.get(&key))
        .and_then(|s| s.get("coords_mm"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let a = r.as_array()?;
                    Some([
                        a.first()?.as_f64()?,
                        a.get(1)?.as_f64()?,
                        a.get(2)?.as_f64()?,
                    ])
                })
                .collect()
        })
        .unwrap_or_default()
}

struct Structure {
    centres: Vec<[f64; 3]>,
    inverse: fastwarp_core::Mat4,
    dims: [usize; 3],
    inside: Vec<bool>,
    voxel_mm: f64,
}

fn load_structure(path: &Path, side: Side) -> Result<Option<Structure>, String> {
    let (vol, aff) = load_volume_f64(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (nx, ny, nz) = vol.dim();
    let kept = |i: usize, j: usize, k: usize, v: f64| -> bool {
        if !v.is_finite() {
            return false;
        }
        let x = apply_rowmajor(&aff, [i as f64, j as f64, k as f64])[0];
        match side {
            Side::Right => x >= 0.0,
            Side::Left => x <= 0.0,
        }
    };
    let mut max = 0f64;
    for ((i, j, k), v) in vol.indexed_iter() {
        if kept(i, j, k, *v) {
            max = max.max(*v);
        }
    }
    if max <= 0.0 {
        return Ok(None);
    }
    let cut = 0.5 * max;
    let mut centres = Vec::new();
    let mut inside = vec![false; nx * ny * nz];
    for ((i, j, k), v) in vol.indexed_iter() {
        if kept(i, j, k, *v) && *v > cut {
            centres.push(apply_rowmajor(&aff, [i as f64, j as f64, k as f64]));
            inside[i + nx * (j + ny * k)] = true;
        }
    }
    let inverse =
        invert_rowmajor(&aff).ok_or_else(|| format!("{}: singular affine", path.display()))?;
    let spacing = voxel_spacing_rowmajor(&aff);
    Ok(Some(Structure {
        centres,
        inverse,
        dims: [nx, ny, nz],
        inside,
        voxel_mm: (spacing[0] + spacing[1] + spacing[2]) / 3.0,
    }))
}

impl Structure {
    fn distance(&self, p: [f64; 3]) -> f64 {
        self.centres
            .iter()
            .map(|c| ((c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2) + (c[2] - p[2]).powi(2)).sqrt())
            .fold(f64::INFINITY, f64::min)
    }

    fn contains(&self, p: [f64; 3]) -> bool {
        let q = apply_rowmajor(&self.inverse, p);
        let idx = [q[0].round(), q[1].round(), q[2].round()];
        if idx
            .iter()
            .zip(self.dims)
            .any(|(v, n)| *v < 0.0 || *v >= n as f64)
        {
            return false;
        }
        let (i, j, k) = (idx[0] as usize, idx[1] as usize, idx[2] as usize);
        self.inside[i + self.dims[0] * (j + self.dims[1] * k)]
    }
}

type Matrix = Vec<Vec<f64>>;

struct Distances {
    conmat: [Matrix; 2],
    hull: [Matrix; 2],
    voxel: [Matrix; 2],
}

fn compute_distances(
    reco: &Value,
    atlas: &StatsAtlas,
    cancel: Option<&AtomicBool>,
) -> Result<Distances, String> {
    let a = atlas.entries.len();
    let mut out = Distances {
        conmat: [Vec::new(), Vec::new()],
        hull: [Vec::new(), Vec::new()],
        voxel: [Vec::new(), Vec::new()],
    };
    for side in Side::ALL {
        let s = side.index();
        let contacts = contact_coords(reco, side);
        let nan_rows = || vec![vec![f64::NAN; a]; contacts.len()];
        out.conmat[s] = nan_rows();
        out.hull[s] = nan_rows();
        out.voxel[s] = nan_rows();
        if contacts.is_empty() {
            continue;
        }
        for (ai, entry) in atlas.entries.iter().enumerate() {
            if cancelled(cancel) {
                return Err(CANCELLED_MESSAGE.to_owned());
            }
            let Some(path) = atlas.path_for(entry, side).filter(|p| p.is_file()) else {
                continue;
            };
            let Some(structure) = load_structure(&path, side)? else {
                continue;
            };
            for (ci, p) in contacts.iter().enumerate() {
                let d = structure.distance(*p);
                if !d.is_finite() {
                    continue;
                }
                out.conmat[s][ci][ai] = d;
                out.hull[s][ci][ai] = if structure.contains(*p) { 0.0 } else { d };
                out.voxel[s][ci][ai] = if d < structure.voxel_mm { 0.0 } else { d };
            }
        }
    }
    Ok(out)
}

struct PassResult {
    state: DistanceState,
    backed_up: bool,
    note: Option<Note>,
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn contact_distance_pass(
    folder: &Path,
    subject: &str,
    reco: &Value,
    reco_file: &Path,
    atlas: &StatsAtlas,
    cancel: Option<&AtomicBool>,
) -> Result<PassResult, String> {
    let path = stats_path(folder, subject);
    let skipped = |note: Note| PassResult {
        state: DistanceState::Skipped,
        backed_up: false,
        note: Some(note),
    };
    let names = atlas.names();
    let (mut stats, existed) = if path.is_file() {
        match read_ea_stats(&path) {
            Ok(r) if r.unsupported.is_empty() => (r.stats, true),
            Ok(r) => {
                return Ok(skipped(Note::coded(
                    "W_STATS_MAT_SKIPPED",
                    format!(
                        "{subject}: {} holds values that are not decoded ({}); contact distances not written",
                        path.display(),
                        r.unsupported.join(", ")
                    ),
                )))
            }
            Err(e) => {
                return Ok(skipped(Note::coded(
                    "W_STATS_MAT_SKIPPED",
                    format!("{subject}: {e}; contact distances not written"),
                )))
            }
        }
    } else {
        (EaStats::default(), false)
    };
    let same_atlas = stats.atlas_names == names;
    let current = existed
        && same_atlas
        && !stats.conmat.is_empty()
        && modified(reco_file)
            .zip(modified(&path))
            .is_none_or(|(r, s)| r <= s);
    if current {
        return Ok(PassResult {
            state: DistanceState::Kept,
            backed_up: false,
            note: None,
        });
    }
    let distances = compute_distances(reco, atlas, cancel)?;
    let mut backed_up = false;
    if existed && !same_atlas && !stats.atlas_names.is_empty() {
        let backup = path.with_file_name(format!("{subject}_desc-stats_backup.mat"));
        std::fs::copy(&path, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
        backed_up = true;
        stats.stimulation.clear();
    }
    stats.atlas_names = names;
    stats.atlas_types = atlas
        .entries
        .iter()
        .map(|e| e.kind.map_or(f64::NAN, |k| k as f64))
        .collect();
    stats.patname = vec![[subject.to_owned(), subject.to_owned()]];
    stats.conmat = vec![distances.conmat];
    stats.conmat_inside_hull = vec![distances.hull];
    stats.conmat_inside_vox = vec![distances.voxel];
    let bytes = mat5_bytes(&[(EA_STATS_VARIABLE.to_owned(), stats.to_mat())], true)
        .map_err(|e| format!("cannot encode {}: {e}", path.display()))?;
    write_replacing(&path, &bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(PassResult {
        state: DistanceState::Computed,
        backed_up,
        note: None,
    })
}

fn extract_code(message: &str) -> Option<String> {
    let start = message.find("E_")?;
    let code: String = message[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (code.len() > 2).then_some(code)
}

enum VatResult {
    Done { computed: usize },
    NotRun(String),
    Failed(String),
    Cancelled,
}

fn run_vat_label(
    opts: &RunOptions,
    params: &VatParams,
    cancel: Option<&AtomicBool>,
    log: &mut dyn FnMut(String),
) -> VatResult {
    let sub = match opts.resolve_subject() {
        Ok(s) => s,
        Err(e) => return VatResult::Failed(e.to_string()),
    };
    let (tx, rx) = mpsc::channel::<RunEvent>();
    let joined = std::thread::scope(|scope| {
        let sub_ref = &sub;
        let handle = scope.spawn(move || {
            let result = run_vat_with(sub_ref, opts, params, cancel, Some(&tx));
            drop(tx);
            result
        });
        for ev in rx {
            if let RunEvent::LogLine { line } = ev {
                log(line);
            }
        }
        handle.join()
    });
    match joined {
        Err(_) => VatResult::Failed("the vat stage panicked".to_owned()),
        Ok(Err(PipelineError::Cancelled)) => VatResult::Cancelled,
        Ok(Err(e)) => VatResult::Failed(e.to_string()),
        Ok(Ok(StageOutcome::Skipped { reason, .. })) => VatResult::NotRun(reason),
        Ok(Ok(StageOutcome::Completed(token))) => {
            let computed = token
                .strip_prefix("ok:vat:")
                .and_then(|r| r.split('/').next())
                .and_then(|c| c.parse::<usize>().ok())
                .unwrap_or(0);
            VatResult::Done { computed }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Calculate,
    DistancesOnly,
}

struct Prepared {
    stim: Result<Stimulation, (Option<&'static str>, String)>,
}

fn prepare_settings(
    record: &GroupRecord,
    p: &PatientEntry,
    options: &CalcOptions,
) -> Result<Stimulation, (Option<&'static str>, String)> {
    let label = record.stim_label();
    let folder = record
        .patient_folder(p)
        .ok_or((None, "the dataset of the patient is not listed".to_owned()))?;
    let sid = subject_id(&p.subject);
    let existing = load_stimulation(&folder, sid, &label);
    if options.sync_settings {
        if let Some(from) = options.copy_settings_from.as_deref() {
            if let Ok(mut stim) = load_stimulation(&folder, sid, from) {
                stim.label.clone_from(&label);
                stim.subject = sid.to_owned();
                if existing.as_ref().ok() != Some(&stim) {
                    save_stimulation(&folder, &stim)
                        .map_err(|e| (Some("E_VAT_settings"), e.to_string()))?;
                }
                return Ok(stim);
            }
        }
    }
    match existing {
        Ok(s) => Ok(s),
        Err(first) => {
            let Some(from) = options.copy_settings_from.as_deref() else {
                return Err((
                    Some("E_VAT_settings"),
                    format!("no stimulation settings {label}: {first}"),
                ));
            };
            let mut stim = load_stimulation(&folder, sid, from).map_err(|e| {
                (
                    Some("E_VAT_settings"),
                    format!("cannot copy the settings of {from:?}: {e}"),
                )
            })?;
            stim.label.clone_from(&label);
            stim.subject = sid.to_owned();
            save_stimulation(&folder, &stim)
                .map_err(|e| (Some("E_VAT_settings"), e.to_string()))?;
            Ok(stim)
        }
    }
}

fn select_targets(
    record: &GroupRecord,
    selection: Option<&[String]>,
) -> Result<Vec<usize>, GroupError> {
    match selection {
        None => Ok((0..record.patients.len())
            .filter(|&i| record.patients[i].included)
            .collect()),
        Some(list) => {
            let mut out = Vec::new();
            for s in list {
                let i = record
                    .patient_index(s)
                    .ok_or_else(|| GroupError::Invalid(format!("{s} is not in the group")))?;
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            out.sort_unstable();
            Ok(out)
        }
    }
}

struct Job<'a> {
    record: &'a GroupRecord,
    options: &'a CalcOptions,
    mode: Mode,
    atlas: Option<&'a StatsAtlas>,
    cancel: Option<&'a AtomicBool>,
    flips: &'a RefCell<HashMap<String, Result<FlipPaths, FlipBuildError>>>,
}

fn flip_forward(
    job: &Job<'_>,
    space: &str,
    log: &mut dyn FnMut(String),
) -> Result<FlipPaths, FlipBuildError> {
    if let Some(done) = job.flips.borrow().get(space) {
        return done.clone();
    }
    let mut env = FlipEnv::from_environment(
        job.options.run.backend_for(Stage::Normalize),
        job.options.run.ants_max_threads,
    );
    if let Some(root) = &job.options.flip_cache_root {
        env.user_root = Some(root.clone());
    }
    let result = resolve_or_build(space, None, &env, false, job.cancel, log);
    if !matches!(result, Err(FlipBuildError::Cancelled)) {
        job.flips
            .borrow_mut()
            .insert(space.to_owned(), result.clone());
    }
    result
}

fn subject_options(job: &Job<'_>, p: &PatientEntry) -> Option<RunOptions> {
    let ds = job.record.datasets.iter().find(|d| d.id == p.dataset)?;
    let mut o = job.options.run.clone();
    o.bids_dir = PathBuf::from(&ds.root);
    o.subject = subject_id(&p.subject).to_owned();
    o.output_dir = Some(PathBuf::from(&ds.derivatives_root));
    o.vat_labels = vec![job.record.stim_label()];
    o.vat_stats_atlas = job
        .record
        .atlas_set
        .clone()
        .or_else(|| job.options.run.vat_stats_atlas.clone());
    Some(o)
}

fn template_label_dirs(folder: &Path, label: &str) -> Vec<(String, PathBuf)> {
    let mut spaces: Vec<String> = std::fs::read_dir(folder.join("stimulations"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir() && e.file_name() != "native")
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    spaces.sort();
    spaces
        .into_iter()
        .map(|space| {
            let dir = label_dir(folder, &space, label);
            (space, dir)
        })
        .filter(|(_, dir)| dir.is_dir())
        .collect()
}

fn process_subject(
    job: &Job<'_>,
    p: &PatientEntry,
    prepared: &Prepared,
    notes: &mut Vec<Note>,
    log: &mut dyn FnMut(String),
) -> SubjectOutcome {
    let name = p.subject.as_str();
    let fail = |phase, code: Option<&str>, msg: String| {
        SubjectOutcome::new(name, phase).with_message(code, msg)
    };
    let Some(folder) = job.record.patient_folder(p) else {
        return fail(
            CalcPhase::NotRun,
            None,
            "the dataset of the patient is not listed".to_owned(),
        );
    };
    if !folder.is_dir() {
        return fail(
            CalcPhase::NotRun,
            Some("W_LG_PATIENT_MISSING"),
            format!("the folder {} does not exist", folder.display()),
        );
    }
    let reco_file = reco_path(&folder, name);
    let reco: Option<Value> = std::fs::read_to_string(&reco_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let Some(reco) = reco else {
        return fail(CalcPhase::NotRun, None, "no reconstruction".to_owned());
    };
    let mut outcome = SubjectOutcome::new(name, CalcPhase::Kept);
    if job.mode == Mode::Calculate {
        if let Err((code, msg)) = &prepared.stim {
            return fail(
                if code.is_some_and(|c| c == "E_GROUP_MODEL_MIX") {
                    CalcPhase::Failed
                } else {
                    CalcPhase::NotRun
                },
                *code,
                msg.clone(),
            );
        }
    }
    if job.options.contact_distances {
        if let Some(atlas) = job.atlas {
            match contact_distance_pass(&folder, name, &reco, &reco_file, atlas, job.cancel) {
                Ok(r) => {
                    outcome.distances = r.state;
                    outcome.backed_up_stats = r.backed_up;
                    if let Some(n) = r.note {
                        log(format!("dbsloc: warning: {n}"));
                        notes.push(n);
                    }
                }
                Err(m) if m == CANCELLED_MESSAGE => {
                    return fail(CalcPhase::Cancelled, None, m);
                }
                Err(m) => {
                    return fail(CalcPhase::Failed, None, format!("contact distances: {m}"));
                }
            }
        }
    }
    if job.mode == Mode::DistancesOnly {
        outcome.phase = if outcome.distances == DistanceState::Computed {
            CalcPhase::Computed
        } else {
            CalcPhase::Kept
        };
        return outcome;
    }
    let Some(opts) = subject_options(job, p) else {
        return fail(
            CalcPhase::NotRun,
            None,
            "the dataset of the patient is not listed".to_owned(),
        );
    };
    let label = job.record.stim_label();
    let watched: Vec<PathBuf> = template_label_dirs(&folder, &label)
        .into_iter()
        .map(|(_, dir)| dir)
        .collect();
    let watch = ReplaceWatch::new(&watched);
    let vat = run_vat_label(&opts, &job.options.params, job.cancel, log);
    let rewritten = watch.replaced();
    drop(watch);
    let mut stale = FlippedRemoval::default();
    if rewritten {
        for (_, dir) in template_label_dirs(&folder, &label) {
            let r = remove_flipped(&dir);
            stale.removed.extend(r.removed);
            stale.failed.extend(r.failed);
        }
    }
    let outcome = finish_subject(
        job, prepared, name, &folder, &label, outcome, vat, &stale, notes, log,
    );
    let mirrored_again =
        job.options.mirror && matches!(outcome.phase, CalcPhase::Computed | CalcPhase::Kept);
    if !stale.removed.is_empty() && !mirrored_again {
        let n = Note::plain(format!(
            "{name}: {} flipped image(s) of {label} were removed because the originals were \
             rewritten; run Calculate with mirroring to write them again",
            stale.removed.len()
        ));
        log(format!("dbsloc: note: {n}"));
        notes.push(n);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn finish_subject(
    job: &Job<'_>,
    prepared: &Prepared,
    name: &str,
    folder: &Path,
    label: &str,
    mut outcome: SubjectOutcome,
    vat: VatResult,
    stale: &FlippedRemoval,
    notes: &mut Vec<Note>,
    log: &mut dyn FnMut(String),
) -> SubjectOutcome {
    let fail = |phase, code: Option<&str>, msg: String| {
        SubjectOutcome::new(name, phase).with_message(code, msg)
    };
    if !stale.failed.is_empty() {
        let mut msg = format!(
            "the stimulation images of {label} were rewritten, but these flipped images of the \
             old ones could not be deleted and must not be used: {}; close the program that \
             holds them, then delete them or run Calculate with mirroring",
            stale.failed_list()
        );
        let phase = match &vat {
            VatResult::Failed(why) => {
                msg.push_str(&format!("; the vat step also failed: {why}"));
                CalcPhase::Failed
            }
            VatResult::Cancelled => {
                msg.push_str("; the vat step was cancelled");
                CalcPhase::Cancelled
            }
            VatResult::NotRun(why) => {
                msg.push_str(&format!("; the vat step did not run: {why}"));
                CalcPhase::Failed
            }
            VatResult::Done { .. } => CalcPhase::Failed,
        };
        return fail(phase, Some("E_GROUP_FLIP"), msg);
    }
    let computed = match vat {
        VatResult::Done { computed } => computed,
        VatResult::NotRun(reason) => return fail(CalcPhase::NotRun, None, reason),
        VatResult::Cancelled => {
            return fail(CalcPhase::Cancelled, None, CANCELLED_MESSAGE.to_owned())
        }
        VatResult::Failed(msg) => {
            let code = extract_code(&msg);
            return fail(CalcPhase::Failed, code.as_deref(), msg);
        }
    };
    outcome.phase = if computed > 0 || outcome.distances == DistanceState::Computed {
        CalcPhase::Computed
    } else {
        CalcPhase::Kept
    };
    if job.options.mirror {
        let slug = prepared.stim.as_ref().ok().map(|s| s.model.slug());
        let Some(slug) = slug else {
            return fail(
                CalcPhase::Failed,
                Some("E_GROUP_FLIP"),
                "no stimulation model".to_owned(),
            );
        };
        let sid = subject_id(name);
        for (space_name, dir) in template_label_dirs(folder, label) {
            let transform: Option<PathBuf> = match &job.options.flip_transform {
                FlipTransformChoice::Path(p) => Some(p.clone()),
                FlipTransformChoice::PlainFlip => {
                    let n = Note::plain(
                        "mirroring with the plain x flip only: the non-linear part of the flip \
                         transform is not applied",
                    );
                    log(format!("dbsloc: warning: {n}"));
                    notes.push(n);
                    None
                }
                FlipTransformChoice::Auto => match flip_forward(job, &space_name, log) {
                    Ok(p) => Some(p.forward),
                    Err(FlipBuildError::Cancelled) => {
                        return fail(CalcPhase::Cancelled, None, CANCELLED_MESSAGE.to_owned())
                    }
                    Err(FlipBuildError::Failed(why)) => {
                        return fail(
                            CalcPhase::Failed,
                            Some("E_GROUP_FLIP"),
                            format!(
                                "no flip transform for {space_name}: {why}; pass \
                                 --flip-transform FILE to use an existing one or --plain-flip \
                                 for the x flip only (mirroring is disabled until a transform \
                                 exists)"
                            ),
                        )
                    }
                },
            };
            match mirror_label_dir(&dir, sid, slug, transform.as_deref()) {
                Ok(files) => outcome.mirrored.extend(files),
                Err(e) => return fail(CalcPhase::Failed, Some("E_GROUP_FLIP"), e.to_string()),
            }
        }
    }
    outcome.fingerprint = Some(subject_fingerprint(folder, name, label));
    outcome
}

fn resolve_atlas(
    record: &GroupRecord,
    options: &CalcOptions,
    notes: &mut Vec<Note>,
) -> Option<StatsAtlas> {
    let mut sink: Vec<String> = Vec::new();
    let sets =
        bundled_and_external_atlas_sets(options.run.leaddbs_atlases_dir.as_deref(), &mut |l| {
            sink.push(l);
        });
    let choice = record
        .atlas_set
        .clone()
        .or_else(|| options.run.vat_stats_atlas.clone());
    match StatsAtlas::resolve(&sets, choice.as_deref()) {
        Ok(a) => Some(a),
        Err(why) => {
            notes.push(Note::plain(format!(
                "no contact distances and no atlas statistics: {why}"
            )));
            None
        }
    }
}

fn finished(o: SubjectOutcome, progress: &mut dyn FnMut(&CalcEvent)) -> SubjectOutcome {
    progress(&CalcEvent::SubjectFinished(o.clone()));
    o
}

fn run_group(
    record: &mut GroupRecord,
    folder: &Path,
    options: &CalcOptions,
    mode: Mode,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&CalcEvent),
) -> Result<CalcReport, GroupError> {
    let t0 = Instant::now();
    let label = record.stim_label();
    let targets = select_targets(record, options.subjects.as_deref())?;
    if targets.is_empty() {
        return Err(GroupError::Invalid("no patients selected".to_owned()));
    }
    let mut report = CalcReport {
        guid: record.guid.clone(),
        label: label.clone(),
        atlas_set: record.atlas_set.clone(),
        ..CalcReport::default()
    };
    let flips = RefCell::new(HashMap::new());
    let atlas = if options.contact_distances {
        resolve_atlas(record, options, &mut report.notes)
    } else {
        None
    };
    let mut prepared: Vec<Prepared> = Vec::with_capacity(targets.len());
    let mut model = record.stimulation.as_ref().and_then(|s| s.model);
    for &i in &targets {
        if mode == Mode::DistancesOnly {
            prepared.push(Prepared {
                stim: Err((None, String::new())),
            });
            continue;
        }
        let stim = prepare_settings(record, &record.patients[i], options);
        let stim = stim.and_then(|s| match model {
            Some(m) if m != s.model => Err((
                Some("E_GROUP_MODEL_MIX"),
                format!(
                    "the stimulation model {} differs from the group model {}",
                    s.model.slug(),
                    m.slug()
                ),
            )),
            Some(_) => Ok(s),
            None => {
                model = Some(s.model);
                Ok(s)
            }
        });
        prepared.push(Prepared { stim });
    }
    if mode == Mode::Calculate {
        if let (Some(m), Some(s)) = (model, record.stimulation.as_mut()) {
            s.model = Some(m);
        }
    }
    let total = targets.len();
    let mut stop_reason: Option<String> = None;
    for (index, (&pi, prep)) in targets.iter().zip(&prepared).enumerate() {
        let patient = record.patients[pi].clone();
        let name = patient.subject.clone();
        if let Some(why) = &stop_reason {
            let o =
                SubjectOutcome::new(&name, CalcPhase::Cancelled).with_message(None, why.clone());
            report.outcomes.push(finished(o, progress));
            continue;
        }
        if cancelled(cancel) {
            stop_reason = Some(CANCELLED_MESSAGE.to_owned());
            report.interrupted = true;
            let o = SubjectOutcome::new(&name, CalcPhase::Cancelled)
                .with_message(None, CANCELLED_MESSAGE);
            report.outcomes.push(finished(o, progress));
            continue;
        }
        progress(&CalcEvent::SubjectStarted {
            index,
            total,
            subject: name.clone(),
        });
        let started = Instant::now();
        let job = Job {
            record,
            options,
            mode,
            atlas: atlas.as_ref(),
            cancel,
            flips: &flips,
        };
        let mut notes = Vec::new();
        let mut outcome = {
            let mut log = |line: String| {
                progress(&CalcEvent::Log {
                    subject: name.clone(),
                    line,
                });
            };
            process_subject(&job, &patient, prep, &mut notes, &mut log)
        };
        outcome.elapsed_ms = started.elapsed().as_millis();
        report.notes.extend(notes);
        if outcome.phase == CalcPhase::Cancelled {
            report.interrupted = true;
            stop_reason = Some(CANCELLED_MESSAGE.to_owned());
        } else if outcome.phase.is_failure() && options.stop_on_first_failure {
            stop_reason = Some(format!("not run: the batch stopped after {name} failed"));
        }
        report.outcomes.push(finished(outcome, progress));
    }
    if mode == Mode::Calculate && !report.cancelled() {
        let selection: Vec<String> = targets
            .iter()
            .map(|&i| record.patients[i].subject.clone())
            .collect();
        let (path, fingerprint) = write_calc_record(record, folder, &selection)?;
        report.analysis_record = Some(path);
        report.fingerprint = Some(fingerprint);
    }
    report.elapsed_ms = t0.elapsed().as_millis();
    Ok(report)
}

/// *Calculate* (`specs/14` §4.1): per selected patient the contact distance pass, then the `vat` stage for the label `gs_<guid>` against the group's atlas set, then (with `mirror`) the flipped volumes. Subjects run in the group's patient order; a failed subject is reported and the batch goes on unless `stop_on_first_failure`. The record gains the calculation analysis (`atlasstats/calc/analysis.json`, fingerprints of every input, `specs/14` §4.5) and, when unset, the group's VTA model; the caller saves the record.
pub fn run_calculate(
    record: &mut GroupRecord,
    folder: &Path,
    options: &CalcOptions,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&CalcEvent),
) -> Result<CalcReport, GroupError> {
    run_group(record, folder, options, Mode::Calculate, cancel, progress)
}

/// The contact distance pass alone for the selected patients (`specs/14` §6.2: needs the reconstruction only, no VTA).
pub fn run_contact_distances(
    record: &mut GroupRecord,
    folder: &Path,
    options: &CalcOptions,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&CalcEvent),
) -> Result<CalcReport, GroupError> {
    let mut options = options.clone();
    options.contact_distances = true;
    run_group(
        record,
        folder,
        &options,
        Mode::DistancesOnly,
        cancel,
        progress,
    )
}

/// A group *Calculate* job as a server runner or the CLI runs it: opens the group file, runs the batch and saves the record and the Lead Group file. A failure to refresh the Lead Group file is a note, the record is saved first.
pub fn run_calculate_job(
    group_path: &Path,
    options: &CalcOptions,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(&CalcEvent),
) -> Result<CalcReport, GroupError> {
    let mut opened = open_group(group_path)?;
    let mut report = run_calculate(
        &mut opened.record,
        &opened.folder,
        options,
        cancel,
        progress,
    )?;
    report.notes.splice(0..0, opened.notes);
    match save_group(&opened.json_path, &mut opened.record) {
        Ok(notes) => report.notes.extend(notes),
        Err(e) => report.notes.push(Note::plain(format!(
            "the group file was not fully saved: {e}"
        ))),
    }
    Ok(report)
}

#[cfg(test)]
#[path = "calc_tests.rs"]
mod tests;
