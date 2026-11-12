// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use serde_json::{json, Value};

use crate::bids::Subject;
use crate::derivatives;
use crate::display::{has_display, run_viewer, ViewerOutcome};
use crate::options::RunOptions;
use crate::outputs::{partial_of, unlink_outputs, write_replacing};
use crate::paths::{absolute, gui_binary, py_float};
use crate::protocol::RunEvent;
use crate::stages::Stage;

pub const SKIP_NO_DISPLAY_ENV: &str = "DBSLOC_CHECKREG_SKIP_NO_DISPLAY";

/// Outcome of the checkreg QC gate.
///
/// [`GateError::QcPending`] is the typed "waiting for approval" signal: the gate ran and wrote its
/// files, but the method log is not approved. `manifest` carries the human-readable tail of the
/// original message so that `Display` reproduces the exact string the gate returned before this
/// type existed. Every other failure is [`GateError::Message`] with its exact text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    QcPending { stage: String, manifest: String },
    Message(String),
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateError::QcPending { stage, manifest } => {
                write!(f, "{stage}: qc not approved — {manifest}")
            }
            GateError::Message(m) => f.write_str(m),
        }
    }
}

impl From<GateError> for crate::error::PipelineError {
    fn from(e: GateError) -> Self {
        match e {
            GateError::QcPending { stage, manifest } => {
                crate::error::PipelineError::QcPending { stage, manifest }
            }
            GateError::Message(m) => crate::error::PipelineError::Message(m),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SlideSpec {
    pub moving: PathBuf,
    pub fixed: PathBuf,
    pub branch: &'static str,
    pub modality: String,
    pub method_log: PathBuf,
    pub preproc: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewRequest {
    pub stage: Stage,
    pub branch: String,
    pub manifest: PathBuf,
    pub slides: Vec<SlideSpec>,
    pub subj_id: String,
    pub method_log: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    Decided,
    Cancelled,
    Failed(String),
}

pub type ReviewHook = dyn Fn(&ReviewRequest, Option<&AtomicBool>) -> ReviewOutcome + Send + Sync;

pub type PngFn = dyn Fn(&SlideSpec, &Path) -> Result<(), String> + Send + Sync;

#[derive(Clone, Default)]
pub enum PngWriter {
    #[default]
    Viz,
    InProcess(Arc<PngFn>),
}

#[derive(Clone)]
pub struct ReviewSetup {
    pub hook: Arc<ReviewHook>,
    pub png: PngWriter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    NotReviewed,
    Approved,
    Rejected,
}

fn log(tx: Option<&mpsc::Sender<RunEvent>>, line: impl Into<String>) {
    if let Some(t) = tx {
        let _ = t.send(RunEvent::LogLine { line: line.into() });
    }
}

pub fn coregistration_root(sub: &Subject) -> PathBuf {
    derivatives::coreg_root(sub)
}

pub fn normalization_root(sub: &Subject) -> PathBuf {
    derivatives::norm_root(sub)
}

pub fn brainshift_root(sub: &Subject) -> PathBuf {
    derivatives::brainshift_root(sub)
}

pub fn default_coreg_method_log_path(sub: &Subject) -> PathBuf {
    derivatives::coreg_method_log(sub)
}

pub fn default_norm_method_log_path(sub: &Subject) -> PathBuf {
    derivatives::norm_method_log(sub)
}

pub fn default_brainshift_method_log_path(sub: &Subject) -> PathBuf {
    derivatives::brainshift_method_log(sub)
}

pub fn manifest_path(sub: &Subject) -> PathBuf {
    sub.locks_dir().join("checkreg_manifest.json")
}

pub fn method_log_allows_continue(path: &Path) -> bool {
    let Some(text) = std::fs::read_to_string(path).ok() else {
        return false;
    };
    let Ok(d) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let ap = d.get("approval").or_else(|| d.get("approved"));
    let truthy = |v: &Value| matches!(v, Value::Bool(true)) || v.as_f64() == Some(1.0);
    let falsy = |v: &Value| matches!(v, Value::Bool(false)) || v.as_f64() == Some(0.0);
    match ap {
        Some(v) if truthy(v) => true,
        Some(Value::Object(m)) => {
            let vals: Vec<&Value> = m.values().filter(|v| !v.is_null()).collect();
            !vals.is_empty() && !vals.iter().any(|v| falsy(v)) && vals.iter().any(|v| truthy(v))
        }
        _ => false,
    }
}

pub fn method_log_path(sub: &Subject, stage: Stage) -> Option<PathBuf> {
    match stage {
        Stage::Coregister => Some(default_coreg_method_log_path(sub)),
        Stage::Normalize => Some(default_norm_method_log_path(sub)),
        _ => None,
    }
}

pub fn review_state(method_log: &Path) -> ReviewState {
    if method_log_allows_continue(method_log) {
        return ReviewState::Approved;
    }
    let Some(d) = std::fs::read_to_string(method_log)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return ReviewState::NotReviewed;
    };
    let falsy = |v: &Value| matches!(v, Value::Bool(false)) || v.as_f64() == Some(0.0);
    let rejected = match d.get("approval").or_else(|| d.get("approved")) {
        Some(Value::Object(m)) => m.iter().any(|(k, v)| reviewed_key(&d, k) && falsy(v)),
        Some(v) => d.get("reviewed") == Some(&Value::Bool(true)) && falsy(v),
        None => false,
    };
    if rejected {
        ReviewState::Rejected
    } else {
        ReviewState::NotReviewed
    }
}

fn reviewed_key(log: &Value, key: &str) -> bool {
    match log.get("reviewed") {
        Some(Value::Bool(true)) => true,
        Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some(key)),
        _ => false,
    }
}

fn preop_nifti(sub: &Subject, modality: &str) -> Option<PathBuf> {
    if modality == sub.anchor_modality {
        return sub.anchor_in_use().filter(|p| p.is_file());
    }
    sub.preop_raw.get(modality).filter(|p| p.is_file()).cloned()
}

pub fn coreg_slide_specs(sub: &Subject, mlog: &Path) -> Vec<SlideSpec> {
    let Some(fixed) = sub.anchor_in_use().filter(|f| f.is_file()) else {
        return Vec::new();
    };
    let preop = sub
        .coreg
        .anat_preop
        .iter()
        .filter(|(modality, _)| **modality != sub.anchor_modality);
    let postop = sub.coreg.anat_postop.iter();
    preop
        .chain(postop)
        .filter(|(_, image)| image.is_file())
        .map(|(modality, image)| SlideSpec {
            moving: absolute(image),
            fixed: absolute(&fixed),
            branch: "coreg",
            modality: modality.clone(),
            method_log: absolute(mlog),
            preproc: true,
        })
        .collect()
}

fn norm_resampled_template(opts: &RunOptions, sub: &Subject, template: &Path) -> Option<PathBuf> {
    let recorded = crate::stage_impl::read_norm_channels(sub)
        .and_then(|rec| rec.get("norm_isotropic_mm").cloned())
        .map(|v| v.as_f64());
    let mm = match recorded {
        Some(grid) => grid?,
        None => {
            crate::norm_grid::resolve_for_template(opts.norm_isotropic_mm, Some(template), 1, None)
                .mm?
        }
    };
    let dir = derivatives::norm_work_dir(sub);
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(format!("template_is{}mm.nii.gz", py_float(mm)));
    crate::resample::resample_isotropic(template, &out, mm).ok()
}

pub fn norm_slide_specs(sub: &Subject, opts: &RunOptions, mlog: &Path) -> Vec<SlideSpec> {
    let Some(template) = opts.normalization_template.as_ref() else {
        return Vec::new();
    };
    if !template.is_file()
        || !crate::stage_impl::normalization_matches_template(sub, Some(template))
    {
        return Vec::new();
    }
    let mut specs = Vec::new();
    let anchor_out = sub.norm.anat_preop.get(&sub.anchor_modality);
    if let Some(warped) = anchor_out.filter(|w| w.is_file()) {
        let warped_fixed =
            norm_resampled_template(opts, sub, template).unwrap_or_else(|| template.clone());
        specs.push(SlideSpec {
            moving: absolute(warped),
            fixed: absolute(&warped_fixed),
            branch: "norm",
            modality: sub.anchor_modality.clone(),
            method_log: absolute(mlog),
            preproc: true,
        });
    }
    if let Some(spec) = norm_t2_coreg_slide(sub, mlog) {
        specs.push(spec);
    }
    for (key, image) in sub.norm.anat_postop.iter() {
        if image.is_file() {
            let modality = if key == derivatives::CT_KEY {
                "postopCT".to_owned()
            } else {
                format!("postop{key}")
            };
            specs.push(SlideSpec {
                moving: absolute(image),
                fixed: absolute(template),
                branch: "norm",
                modality,
                method_log: absolute(mlog),
                preproc: true,
            });
        }
    }
    specs
}

fn norm_t2_coreg_slide(sub: &Subject, mlog: &Path) -> Option<SlideSpec> {
    let record = crate::stage_impl::read_norm_channels(sub)?;
    let t2 = crate::stage_impl::NORM_T2_CHANNEL;
    let used = record
        .get("channels")?
        .as_array()?
        .iter()
        .any(|c| c.as_str() == Some(t2));
    if !used {
        return None;
    }
    let warped = PathBuf::from(record.get("t2_coreg_warped")?.as_str()?);
    let fixed = preop_nifti(sub, &sub.anchor_modality)?;
    warped.is_file().then(|| SlideSpec {
        moving: absolute(&warped),
        fixed: absolute(&fixed),
        branch: "norm",
        modality: format!("{t2}_to_{}", sub.anchor_modality),
        method_log: absolute(mlog),
        preproc: true,
    })
}

pub fn png_path_for_pair(checkreg_dir: &Path, warped: &Path) -> PathBuf {
    derivatives::checkreg_png(checkreg_dir, warped)
}

fn viz_png(sp: &SlideSpec, tmp_png: &Path) -> Result<(), String> {
    let options = dbsloc_checkreg::GencheckregpairOptions {
        preproc: sp.preproc,
        ..Default::default()
    };
    dbsloc_checkreg::gencheckregpair_png(&sp.moving, &sp.fixed, tmp_png, options).map_err(|e| {
        format!(
            "checkreg: gencheckregpair failed: {:?}",
            e.to_string().trim()
        )
    })
}

pub fn build_slides_with(
    sub: &Subject,
    specs: &[SlideSpec],
    checkreg_dir: &Path,
    writer: &PngWriter,
) -> Result<(), String> {
    let mut slides = Vec::new();
    for sp in specs {
        let png = png_path_for_pair(checkreg_dir, &sp.moving);
        if let Some(parent) = png.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("checkreg: {e}"))?;
        }
        let tmp_png = partial_of(&png);
        unlink_outputs(std::slice::from_ref(&tmp_png)).map_err(|e| format!("checkreg: {e}"))?;
        let written = match writer {
            PngWriter::Viz => viz_png(sp, &tmp_png),
            PngWriter::InProcess(f) => f(sp, &tmp_png)
                .map_err(|e| format!("checkreg: gencheckregpair failed: {:?}", e.trim())),
        };
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp_png);
            return Err(e);
        }
        std::fs::rename(&tmp_png, &png).map_err(|e| format!("checkreg: {e}"))?;
        slides.push(json!({
            "checkreg_png": absolute(&png).to_string_lossy(),
            "moving": sp.moving.to_string_lossy(),
            "fixed": sp.fixed.to_string_lossy(),
            "branch": sp.branch,
            "modality": sp.modality,
            "method_log": sp.method_log.to_string_lossy(),
        }));
    }
    let manifest = json!({
        "manifest_version": 1,
        "subj_id": sub.subj_id,
        "active": 0,
        "subj_dir": sub.subj_dir.to_string_lossy(),
        "slides": slides,
    });
    let mpath = manifest_path(sub);
    if let Some(parent) = mpath.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("checkreg: {e}"))?;
    }
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| format!("checkreg: {e}"))?;
    write_replacing(&mpath, (text + "\n").as_bytes()).map_err(|e| format!("checkreg: {e}"))?;
    Ok(())
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub struct GateEnv {
    pub display: (bool, Option<String>),
    pub skip_no_display: bool,
    pub viewer: Option<(PathBuf, Vec<String>)>,
    pub review: Option<Arc<ReviewHook>>,
    pub png: PngWriter,
}

impl GateEnv {
    pub fn from_system() -> Self {
        GateEnv {
            display: has_display(),
            skip_no_display: env_truthy(SKIP_NO_DISPLAY_ENV),
            viewer: gui_binary().map(|p| (p, vec!["checkreg".to_owned()])),
            review: None,
            png: PngWriter::Viz,
        }
    }

    pub fn from_setup(review: Option<&ReviewSetup>) -> Self {
        let mut env = GateEnv::from_system();
        if let Some(r) = review {
            env.review = Some(Arc::clone(&r.hook));
            env.png = r.png.clone();
        }
        env
    }
}

fn stage_of_prefix(prefix: &str) -> Option<Stage> {
    match prefix {
        "coreg" => Some(Stage::Coregister),
        "norm" => Some(Stage::Normalize),
        "brainshift" => Some(Stage::Brainshift),
        _ => None,
    }
}

#[cfg(test)]
fn viewer_and_gate_with(
    sub: &Subject,
    err_prefix: &str,
    method_log: &Path,
    interactive: bool,
    env: &GateEnv,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    viewer_and_gate_slides(
        sub,
        err_prefix,
        method_log,
        interactive,
        env,
        &[],
        cancel,
        tx,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn viewer_and_gate_slides(
    sub: &Subject,
    err_prefix: &str,
    method_log: &Path,
    interactive: bool,
    env: &GateEnv,
    slides: &[SlideSpec],
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    if let Some(hook) = env.review.as_ref() {
        let Some(stage) = stage_of_prefix(err_prefix) else {
            return Err(GateError::Message(format!(
                "{err_prefix}: review failed: no review for this stage"
            )));
        };
        let request = ReviewRequest {
            stage,
            branch: err_prefix.to_owned(),
            manifest: manifest_path(sub),
            slides: slides.to_vec(),
            subj_id: sub.subj_id.clone(),
            method_log: method_log.to_path_buf(),
        };
        match hook(&request, cancel) {
            ReviewOutcome::Decided => {}
            ReviewOutcome::Cancelled => {
                return Err(GateError::Message("cancelled by user".to_owned()));
            }
            ReviewOutcome::Failed(msg) => {
                return Err(GateError::Message(format!(
                    "{err_prefix}: review failed: {msg}"
                )));
            }
        }
    } else {
        external_gate(sub, err_prefix, interactive, env, cancel, tx)?;
    }
    if !method_log_allows_continue(method_log) {
        let lock_stage = stage_of_prefix(err_prefix).map_or(err_prefix, |s| s.cli_name());
        return Err(GateError::QcPending {
            stage: err_prefix.to_owned(),
            manifest: format!(
                "approve in the viewer or edit {} (field approval); on the command line, run \
                 `dbsloc lock approve <dataset> {} {lock_stage}` (and `--output-dir` if the run \
                 used one) before the next `dbsloc run` so that it continues past this step",
                method_log.display(),
                sub.subj_id
            ),
        });
    }
    Ok(())
}

fn external_gate(
    sub: &Subject,
    err_prefix: &str,
    interactive: bool,
    env: &GateEnv,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    let skip_viewer = !interactive || env.skip_no_display;
    let (display_ok, note) = env.display.clone();
    if let Some(n) = note {
        log(tx, format!("dbsloc: note: {n}"));
    }
    if !display_ok {
        if !skip_viewer {
            return Err(GateError::Message(format!(
                "{err_prefix}: checkreg needs a display (DISPLAY or WAYLAND_DISPLAY on Linux, an \
                 interactive desktop session on Windows) (or set \
                 DBSLOC_CHECKREG_SKIP_NO_DISPLAY=1 to validate an existing log only, or use \
                 dbsloc run --headless)"
            )));
        }
        log(
            tx,
            format!(
                "dbsloc: no display: skipping checkreg viewer; if method log is not \
                 pre-approved, {err_prefix} QC fails"
            ),
        );
    } else if !skip_viewer {
        let (exe, prefix) = env.viewer.clone().ok_or_else(|| {
            GateError::Message(format!(
                "{err_prefix}: dbs-localizer not found; cargo build -p dbsloc-app --release \
                 or set DBSLOC_GUI_BIN"
            ))
        })?;
        let mut cmd = Command::new(exe);
        cmd.args(prefix)
            .arg("--manifest")
            .arg(absolute(&manifest_path(sub)));
        match run_viewer(&mut cmd, cancel) {
            ViewerOutcome::Exited(0) => {}
            ViewerOutcome::Exited(code) => {
                return Err(GateError::Message(format!(
                    "{err_prefix}: checkreg viewer exited {code}"
                )));
            }
            ViewerOutcome::Cancelled => {
                return Err(GateError::Message("cancelled by user".to_owned()));
            }
            ViewerOutcome::Failed(e) => {
                return Err(GateError::Message(format!(
                    "{err_prefix}: checkreg viewer failed to start: {e}"
                )));
            }
        }
    }
    Ok(())
}

static APPROVAL_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static APPROVAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn is_scalar_branch(branch: &str) -> bool {
    matches!(
        branch.to_lowercase().as_str(),
        "norm" | "normalization" | "brainshift"
    )
}

/// Record an approval or rejection in a method log, preserving every other key.
///
/// Branches `norm` / `normalization` / `brainshift` get a scalar `approval` (1 or 0); any other
/// branch (coreg) merges `{modality: 1|0}` into the `approval` dictionary. This is the merge the
/// `dbs-localizer checkreg` viewer performs; it lives here so the pipeline and the server share one
/// implementation. Writes through a uniquely named `.partial-*` file renamed into place.
pub fn apply_approval(
    method_log: &Path,
    branch: &str,
    modality: Option<&str>,
    ok: bool,
) -> std::io::Result<()> {
    set_approval(method_log, branch, modality, ok, true)
}

pub(crate) fn set_approval(
    method_log: &Path,
    branch: &str,
    modality: Option<&str>,
    ok: bool,
    reviewed: bool,
) -> std::io::Result<()> {
    let _guard = APPROVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut root: Value = if method_log.is_file() {
        let text = std::fs::read_to_string(method_log)?;
        if text.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        }
    } else {
        if let Some(parent) = method_log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        json!({})
    };
    if root.is_null() {
        root = json!({});
    }
    let Some(obj) = root.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: method log is not a JSON object", method_log.display()),
        ));
    };
    let val = i32::from(ok);
    if is_scalar_branch(branch) {
        if reviewed {
            obj.insert("reviewed".to_owned(), json!(true));
        } else {
            obj.remove("reviewed");
        }
        obj.insert("approval".to_owned(), json!(val));
    } else {
        let key = modality.unwrap_or("UNSPECIFIED").to_owned();
        let mut dict = match obj.remove("approval") {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        if reviewed {
            let mut keys: Vec<String> = match obj.get("reviewed") {
                Some(Value::Array(list)) => list
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                Some(Value::Bool(true)) => dict.keys().cloned().collect(),
                _ => Vec::new(),
            };
            if !keys.contains(&key) {
                keys.push(key.clone());
            }
            obj.insert("reviewed".to_owned(), json!(keys));
        } else {
            obj.remove("reviewed");
        }
        dict.insert(key, json!(val));
        obj.insert("approval".to_owned(), Value::Object(dict));
    }
    let text = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    let name = method_log
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = APPROVAL_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = method_log.with_file_name(format!(".partial-{name}-{}-{n}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, format!("{text}\n")) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, method_log) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

pub fn apply_decisions(
    method_log: &Path,
    branch: &str,
    decisions: &[(Option<String>, bool)],
) -> Result<(), String> {
    if decisions.is_empty() {
        return Ok(());
    }
    if is_scalar_branch(branch) {
        let all = decisions.iter().all(|(_, ok)| *ok);
        return apply_approval(method_log, branch, None, all)
            .map_err(|e| format!("{}: {e}", method_log.display()));
    }
    for (modality, ok) in decisions {
        apply_approval(method_log, branch, modality.as_deref(), *ok)
            .map_err(|e| format!("{}: {e}", method_log.display()))?;
    }
    Ok(())
}

/// Whether one checkreg slide is approved in its method log.
///
/// Scalar branches follow [`method_log_allows_continue`]. For coreg the slide's `modality` entry of
/// the `approval` dictionary must be truthy; a legacy truthy scalar `approval` approves every slide.
pub fn slide_approved(method_log: &Path, branch: &str, modality: Option<&str>) -> bool {
    if is_scalar_branch(branch) {
        return method_log_allows_continue(method_log);
    }
    let Ok(text) = std::fs::read_to_string(method_log) else {
        return false;
    };
    let Ok(d) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let truthy = |v: &Value| matches!(v, Value::Bool(true)) || v.as_f64() == Some(1.0);
    match d.get("approval").or_else(|| d.get("approved")) {
        Some(Value::Object(m)) => m.get(modality.unwrap_or("UNSPECIFIED")).is_some_and(truthy),
        Some(v) => truthy(v),
        None => false,
    }
}

pub fn slide_rejected(method_log: &Path, branch: &str, modality: Option<&str>) -> bool {
    let Some(d) = std::fs::read_to_string(method_log)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return false;
    };
    let falsy = |v: &Value| matches!(v, Value::Bool(false)) || v.as_f64() == Some(0.0);
    match d.get("approval").or_else(|| d.get("approved")) {
        Some(Value::Object(m)) if !is_scalar_branch(branch) => {
            let key = modality.unwrap_or("UNSPECIFIED");
            reviewed_key(&d, key) && m.get(key).is_some_and(falsy)
        }
        Some(Value::Object(m)) => {
            d.get("reviewed") == Some(&Value::Bool(true)) && m.values().any(falsy)
        }
        Some(v) => d.get("reviewed") == Some(&Value::Bool(true)) && falsy(v),
        None => false,
    }
}

fn prefixed(err: String, prefix: &str) -> String {
    match err.strip_prefix("checkreg:") {
        Some(rest) => format!("{prefix}: checkreg:{rest}"),
        None => err,
    }
}

pub fn run_coreg_checkreg(
    sub: &Subject,
    opts: &RunOptions,
    review: Option<&ReviewSetup>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    let mlog = default_coreg_method_log_path(sub);
    let specs = coreg_slide_specs(sub, &mlog);
    if specs.is_empty() {
        return Err(GateError::Message(
            "coreg: checkreg: no warped/fixed NIfTI pairs under coreg_dir".to_owned(),
        ));
    }
    let dir = derivatives::checkreg_dir(&coregistration_root(sub));
    let env = GateEnv::from_setup(review);
    build_slides_with(sub, &specs, &dir, &env.png)
        .map_err(|e| GateError::Message(prefixed(e, "coreg")))?;
    viewer_and_gate_slides(
        sub,
        "coreg",
        &mlog,
        !opts.headless,
        &env,
        &specs,
        cancel,
        tx,
    )
}

pub fn run_norm_checkreg(
    sub: &Subject,
    opts: &RunOptions,
    review: Option<&ReviewSetup>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    if opts.normalization_template.is_none() {
        return Err(GateError::Message(
            "norm: checkreg: no normalization template in RunOptions".to_owned(),
        ));
    }
    let mlog = default_norm_method_log_path(sub);
    let specs = norm_slide_specs(sub, opts, &mlog);
    if specs.is_empty() {
        return Err(GateError::Message(
            "norm: checkreg: no warped anchor / template pair on disk".to_owned(),
        ));
    }
    let dir = derivatives::checkreg_dir(&normalization_root(sub));
    let env = GateEnv::from_setup(review);
    build_slides_with(sub, &specs, &dir, &env.png)
        .map_err(|e| GateError::Message(prefixed(e, "norm")))?;
    let verdict =
        viewer_and_gate_slides(sub, "norm", &mlog, !opts.headless, &env, &specs, cancel, tx);
    if verdict.is_ok() {
        let _ = std::fs::remove_dir_all(derivatives::norm_work_dir(sub));
    }
    verdict
}

fn apply_recorded_norm_grid(opts: &mut RunOptions, rec: &Value) {
    if let Some(grid) = rec.get("norm_isotropic_mm") {
        opts.norm_isotropic_mm = crate::options::NormGrid::from_recorded(grid.as_f64());
    }
    let recorded = |i: usize| crate::stage_impl::recorded_template(rec, i).filter(|p| p.is_file());
    if let Some(t1) = recorded(0) {
        opts.normalization_template = Some(t1);
    }
    if let Some(t2) = recorded(1) {
        opts.normalization_template_t2 = Some(t2);
    }
}

pub fn prepare_review(
    sub: &Subject,
    opts: &RunOptions,
    stage: Stage,
    png: &PngWriter,
) -> Result<ReviewRequest, String> {
    let (branch, mlog, specs, dir) = match stage {
        Stage::Coregister => {
            let mlog = default_coreg_method_log_path(sub);
            let specs = coreg_slide_specs(sub, &mlog);
            if specs.is_empty() {
                return Err(
                    "coreg: checkreg: no warped/fixed NIfTI pairs under coreg_dir".to_owned(),
                );
            }
            let dir = derivatives::checkreg_dir(&coregistration_root(sub));
            ("coreg", mlog, specs, dir)
        }
        Stage::Normalize => {
            let mut resolved = opts.with_resolved_template();
            if let Some(rec) = crate::stage_impl::read_norm_channels(sub) {
                apply_recorded_norm_grid(&mut resolved, &rec);
            }
            if resolved.normalization_template.is_none() {
                return Err("norm: checkreg: no normalization template in RunOptions".to_owned());
            }
            let mlog = default_norm_method_log_path(sub);
            let specs = norm_slide_specs(sub, &resolved, &mlog);
            if specs.is_empty() {
                return Err("norm: checkreg: no warped anchor / template pair on disk".to_owned());
            }
            let dir = derivatives::checkreg_dir(&normalization_root(sub));
            ("norm", mlog, specs, dir)
        }
        other => {
            return Err(format!(
                "{}: no registration review for this stage",
                other.cli_name()
            ));
        }
    };
    build_slides_with(sub, &specs, &dir, png).map_err(|e| prefixed(e, branch))?;
    Ok(ReviewRequest {
        stage,
        branch: branch.to_owned(),
        manifest: manifest_path(sub),
        slides: specs,
        subj_id: sub.subj_id.clone(),
        method_log: mlog,
    })
}

pub fn finish_review(sub: &Subject, _opts: &RunOptions, stage: Stage) {
    if stage == Stage::Normalize && method_log_allows_continue(&default_norm_method_log_path(sub)) {
        let _ = std::fs::remove_dir_all(derivatives::norm_work_dir(sub));
    }
}

pub struct BrainshiftPaths {
    pub anat_moving: PathBuf,
    pub anat_anchor: PathBuf,
    pub anat_scrf: Option<PathBuf>,
    pub log_method: PathBuf,
}

pub fn run_brainshift_checkreg(
    sub: &Subject,
    opts: &RunOptions,
    bs: &BrainshiftPaths,
    review: Option<&ReviewSetup>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(), GateError> {
    if !bs.anat_anchor.is_file() || !bs.anat_moving.is_file() {
        return Ok(());
    }
    let mlog = bs.log_method.clone();
    let mut specs = vec![SlideSpec {
        moving: absolute(&bs.anat_moving),
        fixed: absolute(&bs.anat_anchor),
        branch: "brainshift",
        modality: "pre_scrf".to_owned(),
        method_log: absolute(&mlog),
        preproc: false,
    }];
    if let Some(scrf) = bs.anat_scrf.as_ref().filter(|p| p.is_file()) {
        specs.push(SlideSpec {
            moving: absolute(scrf),
            fixed: absolute(&bs.anat_anchor),
            branch: "brainshift",
            modality: "post_scrf".to_owned(),
            method_log: absolute(&mlog),
            preproc: false,
        });
    }
    let dir = derivatives::checkreg_dir(&brainshift_root(sub));
    let env = GateEnv::from_setup(review);
    build_slides_with(sub, &specs, &dir, &env.png)
        .map_err(|e| GateError::Message(prefixed(e, "brainshift")))?;
    viewer_and_gate_slides(
        sub,
        "brainshift",
        &mlog,
        !opts.headless,
        &env,
        &specs,
        cancel,
        tx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_chk_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn gate_subject(name: &str) -> (PathBuf, Subject) {
        let d = scratch(name);
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        (d, sub)
    }

    fn fake_viewer(code: i32) -> (PathBuf, Vec<String>) {
        if cfg!(windows) {
            (
                PathBuf::from("cmd"),
                vec!["/C".into(), "exit".into(), code.to_string()],
            )
        } else {
            (
                PathBuf::from("sh"),
                vec!["-c".into(), format!("exit {code}")],
            )
        }
    }

    fn env(display: bool, skip: bool, viewer: Option<(PathBuf, Vec<String>)>) -> GateEnv {
        GateEnv {
            display: (display, None),
            skip_no_display: skip,
            viewer,
            review: None,
            png: PngWriter::Viz,
        }
    }

    #[test]
    fn interactive_without_display_is_an_error() {
        let (root, sub) = gate_subject("nodisplay");
        let log = root.join("log.json");
        std::fs::write(&log, r#"{"approval": 1}"#).unwrap();
        let e = viewer_and_gate_with(
            &sub,
            "coreg",
            &log,
            true,
            &env(false, false, None),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(e.starts_with("coreg: checkreg needs a display"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn headless_or_skip_env_only_checks_the_method_log() {
        let (root, sub) = gate_subject("headless");
        let log = root.join("log.json");
        for (interactive, e) in [
            (false, env(false, false, None)),
            (true, env(false, true, None)),
            (false, env(true, false, None)),
        ] {
            std::fs::write(&log, r#"{"approval": 0}"#).unwrap();
            let err = viewer_and_gate_with(&sub, "norm", &log, interactive, &e, None, None)
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("norm: qc not approved"), "{err}");
            std::fs::write(&log, r#"{"approval": 1}"#).unwrap();
            viewer_and_gate_with(&sub, "norm", &log, interactive, &e, None, None).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn viewer_exit_status_and_missing_viewer() {
        let (root, sub) = gate_subject("viewer");
        let log = root.join("log.json");
        std::fs::write(&log, r#"{"approval": 1}"#).unwrap();
        let e = viewer_and_gate_with(
            &sub,
            "coreg",
            &log,
            true,
            &env(true, false, None),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("dbs-localizer not found"), "{e}");
        let e = viewer_and_gate_with(
            &sub,
            "coreg",
            &log,
            true,
            &env(true, false, Some(fake_viewer(3))),
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(e, "coreg: checkreg viewer exited 3");
        viewer_and_gate_with(
            &sub,
            "coreg",
            &log,
            true,
            &env(true, false, Some(fake_viewer(0))),
            None,
            None,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn method_log_approval_shapes() {
        let d = scratch("mlog");
        let p = d.join("m.json");
        assert!(!method_log_allows_continue(&p));
        for (body, want) in [
            (r#"{"approval": 1}"#, true),
            (r#"{"approval": true}"#, true),
            (r#"{"approved": 1}"#, true),
            (r#"{"approval": 0}"#, false),
            (r#"{"approval": {"CT": 1}}"#, true),
            (r#"{"approval": {"CT": 1, "T2w": 0}}"#, false),
            (r#"{"approval": {"CT": null}}"#, false),
            (r#"{"approval": {}}"#, false),
            ("not json", false),
        ] {
            std::fs::write(&p, body).unwrap();
            assert_eq!(method_log_allows_continue(&p), want, "{body}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_approval_sets_the_field_and_preserves_other_keys() {
        let d = scratch("apply");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"method":"n4","inputs":[1,2]}"#).unwrap();
        apply_approval(&p, "norm", None, true).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["approval"], json!(1));
        assert_eq!(v["method"], json!("n4"));
        assert_eq!(v["inputs"], json!([1, 2]));
        assert!(text.ends_with("}\n"));
        assert!(method_log_allows_continue(&p));
        assert!(slide_approved(&p, "norm", None));
        apply_approval(&p, "brainshift", None, false).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["approval"], json!(0));
        assert_eq!(v["method"], json!("n4"));
        assert!(!slide_approved(&p, "norm", None));
        let nested = d.join("log").join("coregmethod.json");
        apply_approval(&nested, "coreg", Some("CT"), true).unwrap();
        assert!(method_log_allows_continue(&nested));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn coreg_approval_is_per_modality() {
        let d = scratch("percoreg");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"method":"ants"}"#).unwrap();
        apply_approval(&p, "coreg", Some("CT"), true).unwrap();
        assert!(slide_approved(&p, "coreg", Some("CT")));
        assert!(!slide_approved(&p, "coreg", Some("T2w")));
        apply_approval(&p, "coreg", Some("T2w"), false).unwrap();
        assert!(slide_approved(&p, "coreg", Some("CT")));
        assert!(!slide_approved(&p, "coreg", Some("T2w")));
        apply_approval(&p, "coreg", Some("T2w"), true).unwrap();
        assert!(slide_approved(&p, "coreg", Some("T2w")));
        apply_approval(&p, "coreg", Some("CT"), false).unwrap();
        assert!(!slide_approved(&p, "coreg", Some("CT")));
        assert!(slide_approved(&p, "coreg", Some("T2w")));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["approval"], json!({"CT": 0, "T2w": 1}));
        assert_eq!(v["method"], json!("ants"));
        apply_approval(&p, "coreg", None, true).unwrap();
        assert!(slide_approved(&p, "coreg", None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_coreg_approvals_of_one_log_all_land() {
        let d = scratch("concurrent_coreg");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"method":"ants"}"#).unwrap();
        let modalities: Vec<String> = (0..16).map(|i| format!("M{i}")).collect();
        std::thread::scope(|s| {
            for m in &modalities {
                let p = &p;
                s.spawn(move || apply_approval(p, "coreg", Some(m), true).unwrap());
            }
        });
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        for m in &modalities {
            assert_eq!(v["approval"][m], json!(1), "{m}");
        }
        assert_eq!(
            v["reviewed"].as_array().map(Vec::len),
            Some(modalities.len())
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn legacy_scalar_approval_approves_every_coreg_slide() {
        let d = scratch("legacy");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"approval": 1}"#).unwrap();
        assert!(slide_approved(&p, "coreg", Some("CT")));
        assert!(slide_approved(&p, "coreg", Some("T2w")));
        std::fs::write(&p, r#"{"approval": 0}"#).unwrap();
        assert!(!slide_approved(&p, "coreg", Some("CT")));
        assert!(!slide_approved(&d.join("absent.json"), "coreg", Some("CT")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_approval_rejects_a_non_object_root_and_accepts_null() {
        let d = scratch("roots");
        let p = d.join("m.json");
        for body in ["[]", "3", "\"x\""] {
            std::fs::write(&p, body).unwrap();
            let e = apply_approval(&p, "coreg", Some("CT"), true).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{body}");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
        }
        std::fs::write(&p, "null").unwrap();
        apply_approval(&p, "norm", None, true).unwrap();
        assert!(slide_approved(&p, "norm", None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn concurrent_approvals_do_not_collide_on_the_temp_file() {
        let d = scratch("concurrent");
        let p = d.join("m.json");
        std::fs::write(&p, "{}").unwrap();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let _ = apply_approval(&p, "coreg", Some(&format!("M{i}")), true);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["approval"].is_object());
        let leftovers = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".partial-"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn qc_pending_keeps_the_original_gate_string_and_maps_to_the_pipeline_error() {
        let gate = GateError::QcPending {
            stage: "norm".to_owned(),
            manifest: "approve in the viewer or edit /x/log.json (field approval)".to_owned(),
        };
        assert_eq!(
            gate.to_string(),
            "norm: qc not approved — approve in the viewer or edit /x/log.json (field approval)"
        );
        let p: crate::error::PipelineError = gate.into();
        assert_eq!(
            p.to_string(),
            "norm: qc not approved — approve in the viewer or edit /x/log.json (field approval)"
        );
        assert!(matches!(p, crate::error::PipelineError::QcPending { .. }));
        let p: crate::error::PipelineError = GateError::Message("boom".to_owned()).into();
        assert_eq!(p.to_string(), "boom");
    }

    #[test]
    fn checkreg_pngs_are_named_after_the_image_stem() {
        assert_eq!(
            png_path_for_pair(
                Path::new("c"),
                Path::new("x/sub-1_space-X_desc-preproc_T1w.nii.gz")
            ),
            Path::new("c").join("sub-1_space-X_desc-preproc_T1w.png")
        );
    }

    fn t1_t2_subject(name: &str) -> (PathBuf, Subject) {
        let d = scratch(name);
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T2w.nii.gz"), b"x").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        (d, sub)
    }

    fn resolve(root: &Path) -> Subject {
        crate::bids::resolve_subject_with(root, "1", false).unwrap()
    }

    fn write_norm_log(sub: &Subject, body: Value) {
        std::fs::create_dir_all(derivatives::norm_log_dir(sub)).unwrap();
        std::fs::write(derivatives::norm_method_log(sub), body.to_string()).unwrap();
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn coreg_mr_pairs_are_shown_against_the_anchor() {
        let (root, sub) = t1_t2_subject("mrpair");
        let t2 = derivatives::coreg_t2_image(&sub);
        touch(&t2);
        let sub = resolve(&root);
        let specs = coreg_slide_specs(&sub, &root.join("log.json"));
        assert_eq!(specs.len(), 1, "{specs:?}");
        assert_eq!(specs[0].moving, absolute(&t2));
        assert_eq!(specs[0].fixed, absolute(&sub.anchor_in_use().unwrap()));
        assert_eq!(specs[0].modality, "T2w");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn norm_gate_adds_the_t2_coreg_pair_when_the_record_lists_it() {
        let (root, sub) = t1_t2_subject("normt2");
        let tpl = root.join("t1.nii.gz");
        std::fs::write(&tpl, b"x").unwrap();
        let space = crate::template_catalog::infer_space(&tpl);
        let opts = RunOptions {
            normalization_template: Some(tpl.clone()),
            ..RunOptions::new(&root, "1")
        };
        let warped = derivatives::norm_anchor_image(&sub, &space);
        touch(&warped);
        let t2 = derivatives::coreg_t2_image(&sub);
        touch(&t2);
        let mlog = default_norm_method_log_path(&sub);
        let write = |channels: Value| {
            write_norm_log(
                &sub,
                json!({
                    "template_space": space,
                    "channels": channels,
                    "t2_coreg_warped": t2.to_string_lossy(),
                }),
            );
            resolve(&root)
        };
        let sub1 = write(json!(["T1w"]));
        assert_eq!(norm_slide_specs(&sub1, &opts, &mlog).len(), 1);
        let sub2 = write(json!(["T1w", "T2w"]));
        let specs = norm_slide_specs(&sub2, &opts, &mlog);
        assert_eq!(specs.len(), 2, "{specs:?}");
        assert_eq!(specs[1].modality, "T2w_to_T1w");
        assert_eq!(specs[1].branch, "norm");
        assert_eq!(specs[1].moving, absolute(&t2));
        assert_eq!(specs[1].fixed, absolute(&sub2.anchor_in_use().unwrap()));
        assert_eq!(specs[1].method_log, absolute(&mlog));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn norm_gate_pairs_the_ct_with_the_full_template_and_the_warped_anchor_with_the_resampled_one()
    {
        let (root, sub) = t1_t2_subject("normct");
        touch(&root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii.gz"));
        let tpl = root.join("t1.nii.gz");
        let fine = [
            [0.5, 0.0, 0.0, 0.0],
            [0.0, 0.5, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        crate::nifti_io::write_u8_3d(&tpl, &ndarray::Array3::<u8>::from_elem((8, 8, 8), 3), &fine)
            .unwrap();
        let space = crate::template_catalog::infer_space(&tpl);
        let opts = RunOptions {
            normalization_template: Some(tpl.clone()),
            norm_isotropic_mm: crate::options::NormGrid::Mm(1.0),
            ..RunOptions::new(&root, "1")
        };
        let warped = derivatives::norm_anchor_image(&sub, &space);
        touch(&warped);
        touch(&derivatives::norm_postop_image(
            &resolve(&root),
            "CT",
            &space,
        ));
        write_norm_log(&sub, json!({"template_space": space}));
        let sub = resolve(&root);
        let ct = derivatives::norm_postop_image(&sub, "CT", &space);
        let mlog = default_norm_method_log_path(&sub);
        let specs = norm_slide_specs(&sub, &opts, &mlog);
        assert_eq!(specs.len(), 2, "{specs:?}");
        let resampled = derivatives::norm_work_dir(&sub).join("template_is1.0mm.nii.gz");
        assert!(resampled.is_file());
        assert_eq!(specs[0].moving, absolute(&warped));
        assert_eq!(specs[0].fixed, absolute(&resampled));
        assert_eq!(specs[1].moving, absolute(&ct));
        assert_eq!(specs[1].modality, "postopCT");
        assert_eq!(specs[1].fixed, absolute(&tpl));
        let full = RunOptions {
            norm_isotropic_mm: crate::options::NormGrid::Full,
            ..opts.clone()
        };
        let specs = norm_slide_specs(&sub, &full, &mlog);
        assert_eq!(specs[0].fixed, absolute(&tpl));
        let unchosen = RunOptions {
            norm_isotropic_mm: crate::options::NormGrid::Template,
            ..opts.clone()
        };
        let specs = norm_slide_specs(&sub, &unchosen, &mlog);
        let half = derivatives::norm_work_dir(&sub).join("template_is0.5mm.nii.gz");
        assert_eq!(
            specs[0].fixed,
            absolute(&tpl),
            "the template's own 0.5 mm grid needs no resampled copy"
        );
        assert!(!half.exists());
        write_norm_log(
            &sub,
            json!({"template_space": space, "norm_isotropic_mm": 1.0}),
        );
        let specs = norm_slide_specs(&resolve(&root), &unchosen, &mlog);
        assert_eq!(
            specs[0].fixed,
            absolute(&resampled),
            "the record's grid wins over the options"
        );
        let other_space = json!({"template_space": "SomeOtherSpace"});
        write_norm_log(&sub, other_space);
        assert!(norm_slide_specs(&resolve(&root), &opts, &mlog).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn method_log_paths_follow_the_derivatives_layout() {
        let d = scratch("paths");
        std::fs::create_dir_all(d.join("rawdata/sub-1/ses-preop/anat")).unwrap();
        std::fs::write(
            d.join("rawdata/sub-1/ses-preop/anat/sub-1_ses-preop_T1w.nii.gz"),
            b"x",
        )
        .unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        assert!(default_coreg_method_log_path(&sub)
            .ends_with(Path::new("coregistration/log/sub-1_desc-coregmethod.json")));
        assert!(default_norm_method_log_path(&sub)
            .ends_with(Path::new("normalization/log/sub-1_desc-normmethod.json")));
        assert!(default_brainshift_method_log_path(&sub)
            .ends_with(Path::new("brainshift/log/sub-1_desc-brainshiftmethod.json")));
        assert!(manifest_path(&sub).ends_with(Path::new("locks/checkreg_manifest.json")));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn review_env(hook: Arc<ReviewHook>) -> GateEnv {
        GateEnv {
            display: (false, None),
            skip_no_display: false,
            viewer: None,
            review: Some(hook),
            png: PngWriter::Viz,
        }
    }

    fn tiny_png() -> PngWriter {
        PngWriter::InProcess(Arc::new(|_, out| {
            std::fs::write(out, b"png").map_err(|e| e.to_string())
        }))
    }

    #[test]
    fn review_hook_approving_through_apply_approval_passes_without_a_display() {
        let (root, sub) = gate_subject("hookok");
        let log = root.join("log.json");
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook: Arc<ReviewHook> = {
            let log = log.clone();
            let seen = Arc::clone(&seen);
            Arc::new(move |req, _| {
                seen.lock().unwrap_or_else(|p| p.into_inner()).push((
                    req.stage,
                    req.branch.clone(),
                    req.subj_id.clone(),
                ));
                match apply_approval(&log, "norm", None, true) {
                    Ok(()) => ReviewOutcome::Decided,
                    Err(e) => ReviewOutcome::Failed(e.to_string()),
                }
            })
        };
        viewer_and_gate_with(&sub, "norm", &log, true, &review_env(hook), None, None).unwrap();
        let seen = seen.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            *seen,
            vec![(Stage::Normalize, "norm".to_owned(), "1".to_owned())]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn review_hook_leaving_the_log_unapproved_is_qc_pending() {
        let (root, sub) = gate_subject("hookpending");
        let log = root.join("log.json");
        let hook: Arc<ReviewHook> = Arc::new(|_, _| ReviewOutcome::Decided);
        let err = viewer_and_gate_with(&sub, "coreg", &log, true, &review_env(hook), None, None)
            .unwrap_err();
        assert!(matches!(err, GateError::QcPending { .. }));
        assert_eq!(
            err.to_string(),
            format!(
                "coreg: qc not approved — approve in the viewer or edit {} (field approval); on \
                 the command line, run `dbsloc lock approve <dataset> 1 coreg` (and \
                 `--output-dir` if the run used one) before the next `dbsloc run` so that it \
                 continues past this step",
                log.display()
            )
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn review_hook_sees_the_cancel_flag_and_failures_are_reported() {
        let (root, sub) = gate_subject("hookcancel");
        let log = root.join("log.json");
        std::fs::write(&log, r#"{"approval": 1}"#).unwrap();
        let hook: Arc<ReviewHook> = Arc::new(|_, cancel| {
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                ReviewOutcome::Cancelled
            } else {
                ReviewOutcome::Decided
            }
        });
        let env = review_env(hook);
        let flag = AtomicBool::new(false);
        viewer_and_gate_with(&sub, "norm", &log, true, &env, Some(&flag), None).unwrap();
        flag.store(true, Ordering::SeqCst);
        let e = viewer_and_gate_with(&sub, "norm", &log, true, &env, Some(&flag), None)
            .unwrap_err()
            .to_string();
        assert_eq!(e, "cancelled by user");
        let failing: Arc<ReviewHook> =
            Arc::new(|_, _| ReviewOutcome::Failed("receiver dropped".to_owned()));
        let e = viewer_and_gate_with(&sub, "norm", &log, true, &review_env(failing), None, None)
            .unwrap_err()
            .to_string();
        assert_eq!(e, "norm: review failed: receiver dropped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn in_process_png_writer_writes_pngs_and_the_manifest_without_a_binary() {
        let (root, sub) = gate_subject("inproc");
        let specs: Vec<SlideSpec> = ["a", "b"]
            .iter()
            .map(|m| SlideSpec {
                moving: root.join(format!("sub-1_{m}.nii.gz")),
                fixed: root.join("fixed.nii.gz"),
                branch: "coreg",
                modality: (*m).to_owned(),
                method_log: root.join("log.json"),
                preproc: true,
            })
            .collect();
        let dir = root.join("checkreg");
        build_slides_with(&sub, &specs, &dir, &tiny_png()).unwrap();
        for m in ["a", "b"] {
            assert!(dir.join(format!("sub-1_{m}.png")).is_file());
        }
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path(&sub)).unwrap()).unwrap();
        assert_eq!(manifest["slides"].as_array().map(Vec::len), Some(2));
        assert_eq!(manifest["slides"][1]["modality"], json!("b"));
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".partial-"))
            .count();
        assert_eq!(leftovers, 0);
        let failing = PngWriter::InProcess(Arc::new(|_, _| Err("boom".to_owned())));
        let e = build_slides_with(&sub, &specs, &dir, &failing).unwrap_err();
        assert_eq!(e, "checkreg: gencheckregpair failed: \"boom\"");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn review_state_reads_every_log_shape() {
        let d = scratch("rstate");
        let p = d.join("m.json");
        assert_eq!(review_state(&p), ReviewState::NotReviewed);
        for (body, want) in [
            (r#"{"approval": 1}"#, ReviewState::Approved),
            (r#"{"approval": {"CT": 1}}"#, ReviewState::Approved),
            (r#"{"approval": 0}"#, ReviewState::NotReviewed),
            (r#"{"approval": false}"#, ReviewState::NotReviewed),
            (
                r#"{"approval": 0, "reviewed": true}"#,
                ReviewState::Rejected,
            ),
            (
                r#"{"approval": 1, "reviewed": true}"#,
                ReviewState::Approved,
            ),
            (r#"{"approval": {"CT": 0}}"#, ReviewState::NotReviewed),
            (
                r#"{"approval": {"CT": 0}, "reviewed": true}"#,
                ReviewState::Rejected,
            ),
            (
                r#"{"approval": {"CT": 1, "T2w": 0}}"#,
                ReviewState::NotReviewed,
            ),
            (
                r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": true}"#,
                ReviewState::Rejected,
            ),
            (r#"{"approval": {"CT": null}}"#, ReviewState::NotReviewed),
            (r#"{"approval": {}}"#, ReviewState::NotReviewed),
            (r#"{"method": "ants"}"#, ReviewState::NotReviewed),
            ("not json", ReviewState::NotReviewed),
        ] {
            std::fs::write(&p, body).unwrap();
            assert_eq!(review_state(&p), want, "{body}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn coreg_markers_are_per_modality() {
        let d = scratch("coreg_marker");
        let p = d.join("m.json");
        std::fs::write(
            &p,
            r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": ["CT"]}"#,
        )
        .unwrap();
        assert_eq!(review_state(&p), ReviewState::NotReviewed);
        assert!(!slide_rejected(&p, "coreg", Some("T2w")));
        assert!(!slide_rejected(&p, "coreg", Some("CT")));
        assert!(slide_approved(&p, "coreg", Some("CT")));
        std::fs::write(
            &p,
            r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": ["CT", "T2w"]}"#,
        )
        .unwrap();
        assert_eq!(review_state(&p), ReviewState::Rejected);
        assert!(slide_rejected(&p, "coreg", Some("T2w")));
        std::fs::write(&p, r#"{"approval": {"CT": 1, "T2w": 0}, "reviewed": true}"#).unwrap();
        assert!(slide_rejected(&p, "coreg", Some("T2w")));
        assert!(!slide_rejected(&p, "coreg", Some("CT")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_approval_lists_reviewed_coreg_modalities() {
        let d = scratch("coreg_list");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"approval": {"CT": 0, "T2w": 0}}"#).unwrap();
        apply_approval(&p, "coreg", Some("CT"), true).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["reviewed"], json!(["CT"]));
        assert_eq!(v["approval"], json!({"CT": 1, "T2w": 0}));
        apply_approval(&p, "coreg", Some("CT"), false).unwrap();
        apply_approval(&p, "coreg", Some("T2w"), false).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["reviewed"], json!(["CT", "T2w"]));
        assert_eq!(review_state(&p), ReviewState::Rejected);
        apply_approval(&p, "norm", None, true).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["reviewed"], json!(true));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_non_zero_non_one_value_is_not_rejected() {
        let d = scratch("half");
        let p = d.join("m.json");
        std::fs::write(&p, r#"{"approval": 0.5, "reviewed": true}"#).unwrap();
        assert_eq!(review_state(&p), ReviewState::NotReviewed);
        assert!(!slide_rejected(&p, "norm", None));
        std::fs::write(&p, r#"{"approval": {"CT": 0.5}, "reviewed": ["CT"]}"#).unwrap();
        assert_eq!(review_state(&p), ReviewState::NotReviewed);
        assert!(!slide_rejected(&p, "coreg", Some("CT")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn method_log_path_covers_registration_stages_only() {
        let (root, sub) = gate_subject("mlpath");
        assert_eq!(
            method_log_path(&sub, Stage::Coregister),
            Some(default_coreg_method_log_path(&sub))
        );
        assert_eq!(
            method_log_path(&sub, Stage::Normalize),
            Some(default_norm_method_log_path(&sub))
        );
        assert_eq!(method_log_path(&sub, Stage::Reconstruct), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepare_review_builds_slides_without_gating_and_the_gate_honours_the_setup() {
        let (root, sub) = t1_t2_subject("prepare");
        touch(&derivatives::coreg_t2_image(&sub));
        let sub = resolve(&root);
        let opts = RunOptions {
            headless: true,
            ..RunOptions::new(&root, "1")
        };
        let req = prepare_review(&sub, &opts, Stage::Coregister, &tiny_png()).unwrap();
        assert_eq!(req.stage, Stage::Coregister);
        assert_eq!(req.branch, "coreg");
        assert_eq!(req.subj_id, "1");
        assert_eq!(req.slides.len(), 1);
        assert_eq!(req.slides[0].modality, "T2w");
        assert_eq!(req.method_log, default_coreg_method_log_path(&sub));
        assert_eq!(req.manifest, manifest_path(&sub));
        assert!(req.manifest.is_file());
        assert!(!default_coreg_method_log_path(&sub).exists());
        assert!(prepare_review(&sub, &opts, Stage::Reconstruct, &tiny_png()).is_err());

        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hook: Arc<ReviewHook> = {
            let calls = Arc::clone(&calls);
            Arc::new(move |r, _| {
                calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push((r.stage, r.slides.len()));
                match apply_approval(&r.method_log, &r.branch, Some("T2w"), true) {
                    Ok(()) => ReviewOutcome::Decided,
                    Err(e) => ReviewOutcome::Failed(e.to_string()),
                }
            })
        };
        let setup = ReviewSetup {
            hook,
            png: tiny_png(),
        };
        run_coreg_checkreg(&sub, &opts, Some(&setup), None, None).unwrap();
        assert_eq!(
            *calls.lock().unwrap_or_else(|p| p.into_inner()),
            vec![(Stage::Coregister, 1)]
        );
        assert_eq!(
            review_state(&default_coreg_method_log_path(&sub)),
            ReviewState::Approved
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finish_review_removes_the_norm_work_dir_only_after_approval() {
        let (root, sub) = gate_subject("finish");
        let opts = RunOptions::new(&root, "1");
        let work = derivatives::norm_work_dir(&sub);
        touch(&work.join("template_is1.0mm.nii.gz"));
        write_norm_log(&sub, json!({"approval": 0}));
        finish_review(&sub, &opts, Stage::Normalize);
        assert!(work.is_dir());
        write_norm_log(&sub, json!({"approval": 1}));
        finish_review(&sub, &opts, Stage::Coregister);
        assert!(work.is_dir());
        finish_review(&sub, &opts, Stage::Normalize);
        assert!(!work.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_decisions_ands_scalar_branches_and_maps_coreg_per_modality() {
        let d = scratch("decisions");
        let p = d.join("m.json");
        let dec =
            |v: &[bool]| -> Vec<(Option<String>, bool)> { v.iter().map(|b| (None, *b)).collect() };
        apply_decisions(&p, "norm", &dec(&[false, true, true])).unwrap();
        assert_eq!(review_state(&p), ReviewState::Rejected);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["approval"], json!(0));
        assert_eq!(v["reviewed"], json!(true));
        apply_decisions(&p, "norm", &dec(&[true, true, true])).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["approval"], json!(1));
        apply_decisions(&p, "brainshift", &dec(&[true, false])).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["approval"], json!(0));
        let c = d.join("coreg.json");
        apply_decisions(
            &c,
            "coreg",
            &[
                (Some("CT".to_owned()), true),
                (Some("T2w".to_owned()), false),
            ],
        )
        .unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&c).unwrap()).unwrap();
        assert_eq!(v["approval"], json!({"CT": 1, "T2w": 0}));
        assert_eq!(v["reviewed"], json!(["CT", "T2w"]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prepare_review_uses_the_grid_the_normalization_was_run_with() {
        let (root, sub) = t1_t2_subject("prepgrid");
        let tpl = root.join("t1.nii.gz");
        let fine = [
            [0.5, 0.0, 0.0, 0.0],
            [0.0, 0.5, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        crate::nifti_io::write_u8_3d(&tpl, &ndarray::Array3::<u8>::from_elem((8, 8, 8), 3), &fine)
            .unwrap();
        let space = crate::template_catalog::infer_space(&tpl);
        let warped = derivatives::norm_anchor_image(&sub, &space);
        touch(&warped);
        write_norm_log(
            &sub,
            json!({
                "template_space": space,
                "norm_isotropic_mm": 1.0,
                "template": [tpl.to_string_lossy()],
            }),
        );
        let sub = resolve(&root);
        let opts = RunOptions {
            normalization_template: Some(tpl.clone()),
            norm_isotropic_mm: crate::options::NormGrid::Full,
            ..RunOptions::new(&root, "1")
        };
        let req = prepare_review(&sub, &opts, Stage::Normalize, &tiny_png()).unwrap();
        let resampled = derivatives::norm_work_dir(&sub).join("template_is1.0mm.nii.gz");
        assert!(resampled.is_file());
        assert_eq!(req.slides[0].fixed, absolute(&resampled));
        let _ = std::fs::remove_dir_all(&root);
    }
}
