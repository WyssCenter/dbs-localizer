// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pure-Rust pipeline stage bodies — no Python required.
//!
//! Each `run_*` function takes a resolved [`Subject`], [`RunOptions`], and
//! optional cancellation + log handles, and returns `PipelineResult<StageOutcome>`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ndarray::Array3;
use serde_json::{json, Map, Value};

use dbsloc_reconstruct::native_frame::{implausible_message, resolve_fiducials, SideGeometry};
use dbsloc_reconstruct::{
    check_side, check_tilt, composite_mean_3d, electrode_by_name, trace_fiducials_flat,
    ElectrodeGeometry, PlausibilityLimits, TracConfig, WorldBounds, MAX_TILT_FROM_VERTICAL_DEG,
};
use fastwarp_core::Mat4;
use fastwarp_reg::apply::Interpolation;
use fastwarp_reg::linear::{LinearRecipe, LinearStageResult};
use fastwarp_reg::n4::{N4Outcome, N4Params};
use fastwarp_reg::preprocess::preprocess_volume_no_winsorize;
use fastwarp_reg::register::write_linear_output;
use fastwarp_reg::transform::{
    read_transform_file, write_composite_pair, AffineTransform, CompositePair, TransformChain,
    TransformElem,
};
use fastwarp_reg::{MaskVolume, Volume};

use crate::ants_argv::{
    build_ants_apply_transforms_argv, build_ants_ct_coarse_argv, build_ants_ct_coreg_argv,
    build_ants_mr_coreg_argv, build_ants_syn_argv, build_ants_syn_multispectral_argv,
    build_n4_argv,
};
use crate::bids::Subject;
use crate::bundled_bin::find_binary;
use crate::checkreg::{run_brainshift_checkreg, BrainshiftPaths};
use crate::coreg_multistart::{
    ct_head_centroid_lps, fixed_head_centroid_lps, fixed_mask_path, fixed_registration_mask,
    load_volume, load_volume_with_header, multistart_record, multistart_work_paths,
    pitch_rotation_lps, select_best_start, write_fixed_mask, write_itk_affine_text,
    write_multistart_record, DiagnosticTracker, MultistartError, MultistartInit, StartPaths,
    PITCH_STARTS_DEG,
};
use crate::ct_recon::{
    reconstruct_ct, CtMethod, CtReconError, CtReconRequest, WarpRoute, ORIENTATION_UNRESOLVED,
    SIDE_KEYS, SIDE_KEYS_CONVENTION, SIDE_LEFT, SIDE_RIGHT, TRACER_TRAC_CORE,
};
use crate::derivatives::{
    self, ANTS_AFFINE_SUFFIX, ANTS_COMPOSITE_SUFFIX, ANTS_INVERSE_COMPOSITE_SUFFIX,
    ANTS_WARPED_SUFFIX,
};
use crate::display::has_display;
use crate::error::{PipelineError, PipelineResult};
use crate::manual_correction::{EditSpace, KeptSides};
use crate::nifti_io::load_volume_f64;
use crate::norm_grid::ResolvedNormGrid;
use crate::options::{NormGrid, NormMethod, RunOptions, NORM_METHOD_ANTS_SYN};
use crate::outputs::{unlink_outputs, write_replacing, Staged};
use crate::paths::{absolute, gui_binary, py_float};
use crate::protocol::{RunEvent, StageOutcome};
use crate::reg_backend::{RegBackend, RegChoice, RegEngine, Resolution};

fn exit_code(status: &ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    status.code().unwrap_or(-1)
}

fn tool_name(cmd: &Command) -> String {
    Path::new(cmd.get_program()).file_stem().map_or_else(
        || "process".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    )
}

const STDERR_TAIL_CHARS: usize = 1500;

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::SeqCst))
}

fn pump_lines<R: std::io::Read>(
    reader: R,
    tx: mpsc::Sender<RunEvent>,
    tail: Option<std::sync::Arc<std::sync::Mutex<String>>>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if let Some(t) = tail.as_ref() {
                    let mut t = t.lock().unwrap_or_else(|p| p.into_inner());
                    t.push_str(&line);
                    t.push('\n');
                    if t.len() > 4 * STDERR_TAIL_CHARS {
                        let cut = t.len() - 2 * STDERR_TAIL_CHARS;
                        let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(0);
                        t.drain(..cut);
                    }
                }
                let _ = tx.send(RunEvent::LogLine { line });
            }
        }
    }
}

fn tail_of(s: &str) -> String {
    let s = s.trim();
    if s.len() <= STDERR_TAIL_CHARS {
        return s.to_owned();
    }
    let start = s.len() - STDERR_TAIL_CHARS;
    let start = (start..s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    s[start..].to_owned()
}

/// Spawn `cmd`, stream stdout/stderr as `RunEvent::LogLine`, respect cancellation.
pub(crate) fn spawn_logged(
    cmd: &mut Command,
    _stage: &str,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let (mut shortened, _aliases) = crate::outputs::shorten_long_inputs(cmd);
    let cmd = &mut shortened;
    let tool = tool_name(cmd);
    let (sink, drain) = match tx {
        Some(t) => (t.clone(), None),
        None => {
            let (t, r) = mpsc::channel::<RunEvent>();
            let h = thread::spawn(move || {
                for ev in r {
                    if let RunEvent::LogLine { line } = ev {
                        eprintln!("{line}");
                    }
                }
            });
            (t, Some(h))
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::outputs::hide_console_window(cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| PipelineError::Io(format!("{tool}: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PipelineError::Io("missing stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PipelineError::Io("missing stderr".into()))?;
    let tail = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let tx_out = sink.clone();
    let out_handle = thread::spawn(move || pump_lines(stdout, tx_out, None));
    let tx_err = sink.clone();
    let tail_err = std::sync::Arc::clone(&tail);
    let err_handle = thread::spawn(move || pump_lines(stderr, tx_err, Some(tail_err)));
    drop(sink);
    let finish_drain = |d: Option<thread::JoinHandle<()>>| {
        if let Some(h) = d {
            let _ = h.join();
        }
    };

    let status = loop {
        if cancelled(cancel) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_handle.join();
            let _ = err_handle.join();
            finish_drain(drain);
            return Err(PipelineError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_handle.join();
                let _ = err_handle.join();
                finish_drain(drain);
                return Err(PipelineError::Io(e.to_string()));
            }
        }
    };
    let _ = out_handle.join();
    let _ = err_handle.join();
    finish_drain(drain);

    if !status.success() {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        let stderr_tail = tail_of(&tail.lock().unwrap_or_else(|p| p.into_inner()));
        return Err(PipelineError::ToolFailed {
            tool,
            code: exit_code(&status),
            stderr_tail,
        });
    }
    Ok(())
}

pub(crate) fn log(tx: Option<&mpsc::Sender<RunEvent>>, line: impl Into<String>) {
    if let Some(t) = tx {
        let _ = t.send(RunEvent::LogLine { line: line.into() });
    }
}

fn ants_env(max_threads: Option<u32>) -> Option<std::collections::HashMap<String, String>> {
    max_threads.map(|n| {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "ITK_GLOBAL_DEFAULT_NUMBER_OF_THREADS".into(),
            n.max(1).to_string(),
        );
        m
    })
}

pub(crate) fn require_binary(name: &str, stage: &str) -> PipelineResult<PathBuf> {
    find_binary(name).ok_or_else(|| {
        PipelineError::Message(format!("{stage}: {name} not found (bundled or PATH)"))
    })
}

pub(crate) fn file_name_of(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn suffixed(prefix: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", prefix.display()))
}

pub(crate) fn stage_lock(sub: &Subject, stage: &str) -> PathBuf {
    crate::locks::stage_approval_path(&sub.locks_dir(), stage)
}

pub(crate) fn command_from(argv: &[String], opts: &RunOptions) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(env) = ants_env(opts.ants_max_threads) {
        cmd.envs(env);
    }
    cmd
}

pub(crate) fn require_3d(path: &Path, what: &str) -> PipelineResult<()> {
    let hdr = nifti::NiftiHeader::from_file(path)
        .map_err(|e| PipelineError::Message(format!("{}: {e}", path.display())))?;
    let ndim = usize::from(hdr.dim[0]).min(7);
    if (4..=ndim).any(|i| hdr.dim[i] > 1) {
        let shape: Vec<u16> = hdr.dim[1..=ndim].to_vec();
        return Err(PipelineError::Message(format!(
            "{}: need 3D (or 4D with last dim 1) for {what}, got {shape:?}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) struct PreprocessTools<'a> {
    pub n4: Option<PathBuf>,
    pub choice: crate::reg_backend::N4Choice,
    pub threads: Option<u32>,
    pub cancel: Option<&'a AtomicBool>,
}

pub fn run_preprocess(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let choice =
        crate::reg_backend::choose_n4_for_run(opts.backend_for(crate::stages::Stage::Preprocess))
            .map_err(|e| PipelineError::Message(format!("preprocess: {e}")))?;
    let tools = PreprocessTools {
        n4: if choice.engine.is_none() {
            Some(require_binary("N4BiasFieldCorrection", "preprocess")?)
        } else {
            None
        },
        choice,
        threads: opts.ants_max_threads,
        cancel,
    };
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, "preprocess", cancel, tx)
    };
    preprocess_with(sub, &tools, &mut exec, tx)
}

fn n4_params_json(params: &N4Params) -> Value {
    json!({
        "shrink_factor": params.shrink_factor,
        "fitting_levels": params.fitting_levels,
        "iterations": params.iterations,
        "spline_order": params.spline_order,
        "number_of_control_points": params.number_of_control_points,
        "number_of_histogram_bins": params.number_of_histogram_bins,
        "wiener_filter_noise": params.wiener_filter_noise,
        "bias_field_fwhm": params.bias_field_fwhm,
        "convergence_threshold": params.convergence_threshold,
    })
}

fn n4_levels_json(outcome: &N4Outcome) -> Value {
    json!(outcome
        .levels
        .iter()
        .map(|l| json!({
            "level": l.level,
            "iterations": l.iterations,
            "control_points": l.control_points,
            "grid_size": l.grid_size,
            "convergence": l.convergence,
        }))
        .collect::<Vec<_>>())
}

/// The in-process N4 (`--reg-backend wgpu`): loads the raw anchor, runs the engine and stages the
/// corrected image (float32, gzip) plus its JSON sidecar.
fn preprocess_native(
    engine: &dyn crate::reg_backend::N4Engine,
    tools: &PreprocessTools,
    raw: &Path,
    out: &Path,
    record: &Path,
    staged: &Staged,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let total_start = Instant::now();
    log(
        tx,
        format!("[preprocess] N4 in-process on {}", engine.label()),
    );
    require_3d(raw, "preprocess")?;
    let t = Instant::now();
    let input = Volume::<f64>::load_nifti_f64(raw).map_err(|e| {
        PipelineError::Message(format!("preprocess: cannot read {}: {e}", raw.display()))
    })?;
    let read_s = t.elapsed().as_secs_f64();
    let params = N4Params::default();
    let t = Instant::now();
    let outcome = engine
        .run_n4_cancellable(&input, &params, tools.cancel)
        .map_err(|e| {
            if cancelled(tools.cancel) {
                PipelineError::Cancelled
            } else {
                PipelineError::Message(format!("preprocess: N4 on {} failed: {e}", engine.label()))
            }
        })?;
    let n4_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    outcome
        .corrected
        .save_nifti(&staged.tmp(out))
        .map_err(|e| {
            PipelineError::Message(format!("preprocess: writing {}: {e}", file_name_of(out)))
        })?;
    let write_s = t.elapsed().as_secs_f64();
    for level in &outcome.levels {
        log(
            tx,
            format!(
                "[preprocess] N4 level {}: {} iterations, final convergence {:.3e}",
                level.level,
                level.iterations,
                level.convergence.last().copied().unwrap_or(0.0)
            ),
        );
    }

    let mut timings = Map::new();
    timings.insert("read_s".into(), json!(read_s));
    timings.insert("n4_s".into(), json!(n4_s));
    timings.insert("write_s".into(), json!(write_s));
    timings.insert("total_s".into(), json!(total_start.elapsed().as_secs_f64()));

    let mut rec = Map::new();
    rec.insert(
        "reg_backend".into(),
        json!(tools.choice.resolution.as_str()),
    );
    rec.insert(
        "reg_requested".into(),
        json!(tools.choice.requested.as_str()),
    );
    if let Resolution::Ants {
        reason: Some(reason),
    } = &tools.choice.resolution
    {
        rec.insert("reg_note".into(), json!(reason));
    }
    if let Some(engine) = tools.choice.engine.as_deref() {
        rec.insert("reg_adapter".into(), engine.describe());
    }
    rec.insert("reg_timings".into(), Value::Object(timings));
    rec.insert("n4".into(), n4_params_json(&params));
    rec.insert("levels".into(), n4_levels_json(&outcome));
    let text = serde_json::to_string_pretty(&Value::Object(rec))?;
    std::fs::write(staged.tmp(record), text + "\n")?;
    Ok(())
}

fn preprocess_ants<'a>(
    n4_bin: &Path,
    raw: &Path,
    out: &Path,
    staged: &Staged,
    exec: &mut ToolExec<'a>,
) -> PipelineResult<()> {
    let argv = build_n4_argv(n4_bin, raw, &staged.tmp(out));
    exec(&argv).map_err(|e| match e {
        PipelineError::ToolFailed { code, .. } => {
            PipelineError::Message(format!("N4 failed with {code}"))
        }
        other => other,
    })
}

pub(crate) fn preprocess_with<'a>(
    sub: &Subject,
    tools: &PreprocessTools<'_>,
    exec: &mut ToolExec<'a>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let raw = sub
        .preop_anchor_raw
        .as_deref()
        .ok_or_else(|| PipelineError::Message("no anchor raw".into()))?;
    if !raw.is_file() {
        return Err(PipelineError::Message("anchor raw file missing".into()));
    }

    std::fs::create_dir_all(&sub.preproc_dir)?;
    let out = derivatives::preproc_image(sub);
    let record = derivatives::preproc_record(sub);
    let precoreg_image = derivatives::precoreg_image(sub);
    let precoreg_record = derivatives::precoreg_record(sub);
    let staged = Staged::mapped(vec![
        (
            out.clone(),
            derivatives::short_image(&sub.preproc_dir, "n4"),
        ),
        (record.clone(), crate::outputs::partial_of(&record)),
        (
            precoreg_image.clone(),
            crate::outputs::partial_of(&precoreg_image),
        ),
        (
            precoreg_record.clone(),
            crate::outputs::partial_of(&precoreg_record),
        ),
    ])?;

    if cancelled(tools.cancel) {
        return Err(PipelineError::Cancelled);
    }

    let native = tools.choice.engine.as_deref().map(|engine| {
        log(
            tx,
            format!("[preprocess] N4 {} → {}", raw.display(), out.display()),
        );
        crate::reg_backend::with_worker_threads(tools.threads, || {
            preprocess_native(engine, tools, raw, &out, &record, &staged, tx)
        })
        .map_err(|e| PipelineError::Message(format!("preprocess: {e}")))
        .and_then(|r| r)
    });

    let res = match native {
        None => {
            let n4 = tools.n4.as_deref().ok_or_else(|| {
                PipelineError::Message(
                    "preprocess: N4BiasFieldCorrection not found (bundled or PATH)".into(),
                )
            })?;
            preprocess_ants(n4, raw, &out, &staged, exec)
        }
        Some(Ok(())) => Ok(()),
        Some(Err(PipelineError::Cancelled)) => Err(PipelineError::Cancelled),
        Some(Err(e)) if tools.choice.requested == RegBackend::Auto => {
            log(
                tx,
                format!(
                    "dbsloc: note: preprocess: registration backend ants: the GPU N4 failed: {e}; \
                     the run continues on ANTs"
                ),
            );
            staged.discard();
            let n4 = match tools.n4.clone() {
                Some(b) => b,
                None => require_binary("N4BiasFieldCorrection", "preprocess")?,
            };
            preprocess_ants(&n4, raw, &out, &staged, exec)
        }
        Some(Err(e)) => Err(e),
    };
    staged.finish(res, Some(&stage_lock(sub, "preprocess")))?;

    log(tx, format!("[preprocess] done: {}", file_name_of(&out)));
    Ok(StageOutcome::done(format!(
        "ok:preprocess:n4:{}",
        file_name_of(&out)
    )))
}

/// The `preprocess` lock rule of `--reg-backend` (`specs/02` §4.3, `specs/04` §5.4): `auto` accepts
/// either backend's approved outputs; an explicit request re-runs when the recorded backend
/// differs; no record counts as `ants`.
pub fn preprocess_approved_outputs_match(sub: &Subject, opts: &RunOptions) -> Result<(), String> {
    if opts.backend_for(crate::stages::Stage::Preprocess) == RegBackend::Auto {
        return Ok(());
    }
    let made_with = std::fs::read_to_string(derivatives::preproc_record(sub))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|rec| {
            rec.get("reg_backend")
                .and_then(Value::as_str)
                .unwrap_or("ants")
                .to_owned()
        })
        .unwrap_or_else(|| "ants".to_owned());
    if made_with != opts.backend_for(crate::stages::Stage::Preprocess).as_str() {
        return Err(format!(
            "the approved outputs were made with the {made_with} registration backend, this run \
             asks for {}",
            opts.backend_for(crate::stages::Stage::Preprocess).as_str()
        ));
    }
    Ok(())
}

pub const COREG_METHOD_CT: &str = "ants_rigid_multistart";
pub const COREG_METHOD_MR: &str = "ants_rigid";

pub(crate) struct CtPaths {
    pub warped: PathBuf,
    pub record: PathBuf,
    pub work_dir: PathBuf,
}

impl CtPaths {
    pub(crate) fn of(sub: &Subject) -> CtPaths {
        CtPaths {
            warped: derivatives::coreg_ct_image(sub),
            record: derivatives::coreg_multistart_record(sub),
            work_dir: derivatives::coreg_multistart_dir(sub),
        }
    }
}

fn coreg_work_prefix(sub: &Subject, key: &str) -> PathBuf {
    derivatives::work_prefix(
        &derivatives::coreg_transform_dir(sub),
        &format!("c{}", derivatives::key_tag(key)),
    )
}

pub(crate) fn norm_work_prefix(sub: &Subject) -> PathBuf {
    derivatives::work_prefix(&derivatives::norm_transform_dir(sub), "nrm")
}

pub(crate) fn postop_work_image(sub: &Subject, key: &str) -> PathBuf {
    derivatives::short_image(&sub.norm_dir, &format!("o{}", derivatives::key_tag(key)))
}

fn image_token(image: &Path) -> String {
    let name = file_name_of(image);
    name.strip_suffix(".nii.gz").unwrap_or(&name).to_owned()
}

fn app_version() -> &'static str {
    crate::version::VERSION
}

fn method_log_base(method: &str, engine: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("method".into(), json!(method));
    m.insert("engine".into(), json!(engine));
    m.insert("approval".into(), json!(0));
    m.insert("custom".into(), json!(false));
    m.insert("version".into(), json!(app_version()));
    m
}

fn write_method_log(path: &Path, log: Map<String, Value>) -> PipelineResult<()> {
    let text = serde_json::to_string_pretty(&Value::Object(log))?;
    write_replacing(
        path,
        (text
            + "
")
        .as_bytes(),
    )?;
    Ok(())
}

pub fn run_coregister(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    coreg_inputs(sub)?;
    let reg =
        crate::reg_backend::choose_for_run(opts.backend_for(crate::stages::Stage::Coregister))
            .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?;
    coregister_with(
        sub,
        reg,
        &mut |reg, moving, fixed, staged| {
            coregister_ct(sub, opts, reg, moving, fixed, staged, cancel, tx)
        },
        &mut |reg, key, moving, fixed, staged| {
            coregister_mr(sub, opts, reg, key, moving, fixed, staged, cancel, tx)
        },
    )
}

type CtJob<'a> = dyn FnMut(&mut RegChoice, &Path, &Path, &Staged) -> PipelineResult<String> + 'a;
type MrJob<'a> =
    dyn FnMut(&mut RegChoice, &str, &Path, &Path, &Staged) -> PipelineResult<String> + 'a;

type CoregInputs<'a> = (Option<&'a Path>, Vec<(&'static str, &'a Path)>, PathBuf);

fn coreg_inputs(sub: &Subject) -> PipelineResult<CoregInputs<'_>> {
    let ct = sub.postop_ct_raw.as_deref().filter(|p| p.is_file());
    let mr_images = sub.postop_mr_images();
    if ct.is_none() && mr_images.is_empty() {
        return Err(PipelineError::Message("no postop CT or MR".into()));
    }
    let fixed = sub
        .anchor_in_use()
        .filter(|p| p.is_file())
        .ok_or_else(|| PipelineError::Message("no anchor anat".into()))?;
    Ok((ct, mr_images, fixed))
}

fn ct_targets(sub: &Subject) -> Vec<(PathBuf, PathBuf)> {
    let paths = CtPaths::of(sub);
    let tmp_prefix = coreg_work_prefix(sub, derivatives::CT_KEY);
    vec![
        (paths.warped, suffixed(&tmp_prefix, ANTS_WARPED_SUFFIX)),
        (
            derivatives::coreg_ct_affine(sub),
            suffixed(&tmp_prefix, ANTS_AFFINE_SUFFIX),
        ),
        (
            paths.record.clone(),
            crate::outputs::partial_of(&paths.record),
        ),
        (
            derivatives::coreg_ct_forward(sub),
            suffixed(&tmp_prefix, ANTS_COMPOSITE_SUFFIX),
        ),
        (
            derivatives::coreg_ct_inverse(sub),
            suffixed(&tmp_prefix, ANTS_INVERSE_COMPOSITE_SUFFIX),
        ),
    ]
}

fn mr_targets(sub: &Subject, key: &str) -> Vec<(PathBuf, PathBuf)> {
    let tmp_prefix = coreg_work_prefix(sub, key);
    vec![
        (
            derivatives::coreg_mr_image(sub, key),
            suffixed(&tmp_prefix, ANTS_WARPED_SUFFIX),
        ),
        (
            derivatives::coreg_mr_affine(sub, key),
            suffixed(&tmp_prefix, ANTS_AFFINE_SUFFIX),
        ),
    ]
}

fn discard_targets(targets: &[(PathBuf, PathBuf)]) {
    for (_, tmp) in targets {
        let _ = std::fs::remove_file(tmp);
    }
}

fn coregister_with(
    sub: &Subject,
    mut reg: RegChoice,
    ct_job: &mut CtJob<'_>,
    mr_job: &mut MrJob<'_>,
) -> PipelineResult<StageOutcome> {
    let (ct, mr_images, fixed) = coreg_inputs(sub)?;
    std::fs::create_dir_all(&sub.coreg_dir)?;
    std::fs::create_dir_all(derivatives::coreg_transform_dir(sub))?;
    std::fs::create_dir_all(derivatives::coreg_log_dir(sub))?;

    let method_log = derivatives::coreg_method_log(sub);
    let mut pairs = Vec::new();
    if ct.is_some() {
        pairs.extend(ct_targets(sub));
    }
    for (key, _) in &mr_images {
        pairs.extend(mr_targets(sub, key));
    }
    pairs.push((method_log.clone(), crate::outputs::partial_of(&method_log)));
    let staged = Staged::mapped(pairs)?;

    let res = (|| -> PipelineResult<String> {
        let mut token: Option<String> = None;
        let mut images = Map::new();
        let mut approval = Map::new();
        if let Some(moving) = ct {
            token = Some(ct_job(&mut reg, moving, &fixed, &staged)?);
            images.insert(
                derivatives::CT_KEY.into(),
                json!({
                    "method": COREG_METHOD_CT,
                    "backend": reg.resolution.as_str(),
                    "image": file_name_of(&derivatives::coreg_ct_image(sub)),
                    "transform": file_name_of(&derivatives::coreg_ct_affine(sub)),
                }),
            );
            approval.insert(derivatives::CT_KEY.into(), json!(0));
        }
        for (key, moving) in &mr_images {
            let name = mr_job(&mut reg, key, moving, &fixed, &staged)?;
            token.get_or_insert(name);
            images.insert(
                (*key).to_owned(),
                json!({
                    "method": COREG_METHOD_MR,
                    "backend": reg.resolution.as_str(),
                    "image": file_name_of(&derivatives::coreg_mr_image(sub, key)),
                    "transform": file_name_of(&derivatives::coreg_mr_affine(sub, key)),
                }),
            );
            approval.insert((*key).to_owned(), json!(0));
        }
        let mut log = method_log_base(
            if ct.is_some() {
                COREG_METHOD_CT
            } else {
                COREG_METHOD_MR
            },
            reg.resolution.as_str(),
        );
        log.insert("approval".into(), Value::Object(approval));
        log.extend(images);
        let text = serde_json::to_string_pretty(&Value::Object(log))?;
        std::fs::write(staged.tmp(&method_log), text + "\n")?;
        Ok(token.unwrap_or_default())
    })();
    let token = staged.finish(res, Some(&stage_lock(sub, "coreg")))?;
    Ok(StageOutcome::done(format!("ok:coreg:{token}")))
}

#[allow(clippy::too_many_arguments)]
fn coregister_ct(
    sub: &Subject,
    opts: &RunOptions,
    reg: &mut RegChoice,
    moving: &Path,
    fixed: &Path,
    staged: &Staged,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<String> {
    let paths = CtPaths::of(sub);
    let tmp_prefix = coreg_work_prefix(sub, derivatives::CT_KEY);

    let native = reg.engine.as_deref().map(|engine| {
        coregister_native(
            engine,
            reg,
            moving,
            fixed,
            &paths,
            staged,
            &tmp_prefix,
            opts.ants_max_threads,
            cancel,
            tx,
        )
    });

    match native {
        None => {
            let ants = require_binary("antsRegistration", "coreg")?;
            run_coregister_ants(
                &ants,
                reg,
                moving,
                fixed,
                &paths,
                staged,
                &tmp_prefix,
                opts,
                cancel,
                tx,
            )?;
        }
        Some(Ok(())) => {}
        Some(Err(PipelineError::Cancelled)) => return Err(PipelineError::Cancelled),
        Some(Err(e)) if reg.requested == RegBackend::Auto => {
            log(
                tx,
                format!(
                    "dbsloc: note: coreg: registration backend ants: the GPU coregistration \
                     failed: {e}; the run continues on ANTs"
                ),
            );
            discard_targets(&ct_targets(sub));
            *reg = RegChoice {
                requested: reg.requested,
                resolution: Resolution::Ants {
                    reason: Some(format!("the GPU coregistration failed: {e}")),
                },
                engine: None,
            };
            let ants = require_binary("antsRegistration", "coreg")?;
            run_coregister_ants(
                &ants,
                reg,
                moving,
                fixed,
                &paths,
                staged,
                &tmp_prefix,
                opts,
                cancel,
                tx,
            )?;
        }
        Some(Err(e)) => return Err(e),
    }
    log(
        tx,
        format!("[coregister] done: {}", file_name_of(&paths.warped)),
    );
    Ok(image_token(&paths.warped))
}

#[allow(clippy::too_many_arguments)]
fn coregister_mr(
    sub: &Subject,
    opts: &RunOptions,
    reg: &mut RegChoice,
    key: &str,
    moving: &Path,
    fixed: &Path,
    staged: &Staged,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<String> {
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, "coregister", cancel, tx)
    };
    coregister_mr_staged(
        sub,
        opts,
        reg,
        key,
        moving,
        fixed,
        &|| require_binary("antsRegistration", "coreg"),
        &mut exec,
        staged,
        cancel,
        tx,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn coregister_mr_with(
    sub: &Subject,
    opts: &RunOptions,
    reg: &mut RegChoice,
    key: &str,
    moving: &Path,
    fixed: &Path,
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<String> {
    std::fs::create_dir_all(&sub.coreg_dir)?;
    std::fs::create_dir_all(derivatives::coreg_transform_dir(sub))?;
    let staged = Staged::mapped(mr_targets(sub, key))?;
    let res = coregister_mr_staged(
        sub, opts, reg, key, moving, fixed, find_ants, exec, &staged, cancel, tx,
    );
    staged.finish(res, Some(&stage_lock(sub, "coreg")))
}

#[allow(clippy::too_many_arguments)]
fn coregister_mr_staged(
    sub: &Subject,
    opts: &RunOptions,
    reg: &mut RegChoice,
    key: &str,
    moving: &Path,
    fixed: &Path,
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    staged: &Staged,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<String> {
    let warped = derivatives::coreg_mr_image(sub, key);
    let tmp_prefix = coreg_work_prefix(sub, key);
    let tmp_warped = staged.tmp(&warped);

    let native = reg.engine.as_deref().map(|engine| {
        coregister_mr_native(
            engine,
            key,
            moving,
            fixed,
            &tmp_prefix,
            &tmp_warped,
            opts.ants_max_threads,
            cancel,
            tx,
        )
    });
    match native {
        None => coregister_mr_ants(
            find_ants,
            exec,
            key,
            moving,
            fixed,
            &tmp_prefix,
            &tmp_warped,
            opts,
            tx,
        )?,
        Some(Ok(())) => {}
        Some(Err(PipelineError::Cancelled)) => return Err(PipelineError::Cancelled),
        Some(Err(e)) if reg.requested == RegBackend::Auto => {
            log(
                tx,
                format!(
                    "dbsloc: note: coreg: registration backend ants: the GPU coregistration of \
                     {key} failed: {e}; the run continues on ANTs"
                ),
            );
            discard_targets(&mr_targets(sub, key));
            *reg = RegChoice {
                requested: reg.requested,
                resolution: Resolution::Ants {
                    reason: Some(format!("the GPU coregistration failed: {e}")),
                },
                engine: None,
            };
            coregister_mr_ants(
                find_ants,
                exec,
                key,
                moving,
                fixed,
                &tmp_prefix,
                &tmp_warped,
                opts,
                tx,
            )?;
        }
        Some(Err(e)) => return Err(e),
    }
    log(tx, format!("[coregister] done: {}", file_name_of(&warped)));
    Ok(image_token(&warped))
}

fn empty_warp(what: &str) -> PipelineError {
    PipelineError::Message(format!(
        "coreg: warped {what} is empty (NIfTI world alignment may be wrong; check post-op {what} \
         vs anchor headers)"
    ))
}

fn check_warped_mr(path: &Path, key: &str) -> PipelineResult<()> {
    if path.is_file() && warped_is_empty(path)? {
        return Err(empty_warp(key));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn coregister_mr_ants(
    find_ants: &dyn Fn() -> PipelineResult<PathBuf>,
    exec: &mut ToolExec<'_>,
    key: &str,
    moving: &Path,
    fixed: &Path,
    tmp_prefix: &Path,
    tmp_warped: &Path,
    opts: &RunOptions,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let ants = find_ants()?;
    log(
        tx,
        format!(
            "[coregister] antsRegistration rigid {} → {} ({key})",
            moving.display(),
            fixed.display()
        ),
    );
    let argv = build_ants_mr_coreg_argv(&ants, moving, fixed, tmp_prefix, &opts.ants_extra);
    exec(&argv)?;
    if !tmp_warped.is_file() {
        return Err(PipelineError::Message(format!(
            "coreg: antsRegistration wrote no warped {key}"
        )));
    }
    check_warped_mr(tmp_warped, key)
}

#[allow(clippy::too_many_arguments)]
fn coregister_mr_native(
    engine: &dyn RegEngine,
    key: &str,
    moving: &Path,
    fixed: &Path,
    tmp_prefix: &Path,
    tmp_warped: &Path,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    log(
        tx,
        format!(
            "[coregister] registration backend wgpu on {} ({key})",
            engine.label()
        ),
    );
    let work = || -> PipelineResult<()> {
        let (fixed_vol, moving_vol) =
            load_reg_volume_pair((fixed, "anchor image"), (moving, "post-op MR"), "coreg")?;
        let out = engine
            .rigid_cancellable(
                &LinearRecipe::cohort_rigid(),
                &fixed_vol,
                &moving_vol,
                cancel,
            )
            .map_err(|e| {
                crate::reg_backend::cancelled_or(e, |e| {
                    native_coreg_err(engine, "the post-op MR rigid registration", e)
                })
            })?;
        write_linear_output(tmp_prefix, &out.total)
            .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?;
        let chain = TransformChain::from_queue(vec![TransformElem::Affine(out.total)]);
        let warped = fastwarp_reg::apply::apply_chain(
            &moving_vol,
            fixed_vol.grid(),
            &chain,
            Interpolation::Linear,
            0.0,
        )
        .map_err(|e| native_coreg_err(engine, "resampling the post-op MR", &e))?;
        let data: Vec<f32> = warped.data().iter().map(|v| *v as f32).collect();
        if data_is_empty(data.iter().map(|v| f64::from(*v))) {
            return Err(empty_warp(key));
        }
        Volume::from_vec(*fixed_vol.grid(), data)
            .map_err(|e| native_coreg_err(engine, "resampling the post-op MR", &e))?
            .save_nifti(tmp_warped)
            .map_err(|e| native_coreg_err(engine, "writing the warped post-op MR", &e))
    };
    crate::reg_backend::with_worker_threads(threads, work)
        .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?
}

/// The CT → anchor multi-start on the GPU (`--reg-backend wgpu`): the same stage flow as the ANTs
/// path, with every registration and apply call made by `fastwarp-reg` and the outputs written under
/// the exact ANTs names.
#[allow(clippy::too_many_arguments)]
fn coregister_native(
    engine: &dyn RegEngine,
    reg: &RegChoice,
    moving: &Path,
    fixed: &Path,
    paths: &CtPaths,
    staged: &Staged,
    tmp_prefix: &Path,
    threads: Option<u32>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    crate::reg_backend::with_worker_threads(threads, || {
        coregister_native_with(
            engine,
            reg,
            moving,
            fixed,
            paths,
            staged,
            tmp_prefix,
            &coarse_rigid_recipe(),
            &full_coreg_recipe(),
            cancel,
            tx,
        )
    })
    .map_err(|e| PipelineError::Message(format!("coreg: {e}")))
    .and_then(|r| r)
}

#[allow(clippy::too_many_arguments)]
fn coregister_native_with(
    engine: &dyn RegEngine,
    reg: &RegChoice,
    moving: &Path,
    fixed: &Path,
    paths: &CtPaths,
    staged: &Staged,
    tmp_prefix: &Path,
    coarse_recipe: &LinearRecipe,
    full_recipe: &LinearRecipe,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let total_start = Instant::now();
    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    log(
        tx,
        format!(
            "[coregister] registration backend wgpu on {}",
            engine.label()
        ),
    );
    let t = Instant::now();
    let (fixed_vol, moving_vol) =
        load_reg_volume_pair((fixed, "anchor image"), (moving, "post-op CT"), "coreg")?;
    let pre_fixed = preprocess_volume_no_winsorize(&fixed_vol)
        .map_err(|e| native_coreg_err(engine, "preprocessing the anchor image", &e))?;
    let pre_moving = preprocess_volume_no_winsorize(&moving_vol)
        .map_err(|e| native_coreg_err(engine, "preprocessing the post-op CT", &e))?;
    let warped_grid = *fixed_vol.grid();
    drop((fixed_vol, moving_vol));
    log(
        tx,
        format!(
            "[coregister] preprocessed CT + anchor in {:.1} s",
            t.elapsed().as_secs_f64()
        ),
    );

    let record_tmp = staged.tmp(&paths.record);
    let mut coarse = |start: &StartPaths,
                      pitch: f64,
                      c_f: [f64; 3],
                      c_m: [f64; 3]|
     -> PipelineResult<CoarseStart> {
        if cancelled(cancel) {
            return Err(PipelineError::Cancelled);
        }
        let t0 = Instant::now();
        let initial = AffineTransform::new(
            pitch_rotation_lps(pitch),
            [c_m[0] - c_f[0], c_m[1] - c_f[1], c_m[2] - c_f[2]],
            c_f,
        );
        let out = match engine.rigid_from_cancellable(
            coarse_recipe,
            &pre_fixed,
            &pre_moving,
            initial,
            cancel,
        ) {
            Ok(out) => out,
            Err(fastwarp_reg::RegError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(e) => {
                return Ok(CoarseStart {
                    metric: None,
                    seconds: t0.elapsed().as_secs_f64(),
                    failure: Some(
                        native_coreg_err(engine, "a coarse multi-start rigid", &e).to_string(),
                    ),
                });
            }
        };
        write_linear_output(&start.coarse_prefix, &out.total)
            .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?;
        Ok(CoarseStart {
            metric: out.final_metric(),
            seconds: t0.elapsed().as_secs_f64(),
            failure: None,
        })
    };
    let init = coreg_multistart_init(
        moving,
        fixed,
        StartOutputs {
            work_dir: &paths.work_dir,
            record: &record_tmp,
        },
        reg,
        tx,
        &mut coarse,
    )?;

    if cancelled(cancel) {
        return Err(PipelineError::Cancelled);
    }
    let mask_vol = load_reg_volume(&init.fixed_mask, "fixed mask", "coreg")?;
    let mask = MaskVolume::from_volume(mask_vol);
    let full = full_recipe.clone().with_mask(mask);
    let initial = read_affine(&init.transform)?;
    log(
        tx,
        "[coregister] masked rigid registration (GPU)".to_owned(),
    );
    let t = Instant::now();
    let out = engine
        .rigid_from_cancellable(&full, &pre_fixed, &pre_moving, initial, cancel)
        .map_err(|e| {
            crate::reg_backend::cancelled_or(e, |e| {
                native_coreg_err(engine, "the masked rigid registration", e)
            })
        })?;
    let full_s = t.elapsed().as_secs_f64();
    drop((pre_fixed, pre_moving));

    write_linear_output(tmp_prefix, &out.total)
        .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?;
    let forward = TransformChain::from_queue(vec![TransformElem::Affine(out.total)]);
    let inverse = TransformChain::from_queue(vec![TransformElem::Affine(
        out.total
            .inverse()
            .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?,
    )]);
    write_composite_pair(
        tmp_prefix,
        &CompositePair {
            forward: forward.clone(),
            inverse,
        },
    )
    .map_err(|e| PipelineError::Message(format!("coreg: {e}")))?;
    let warped = staged.tmp(&paths.warped);
    let warped_moving = load_reg_volume(moving, "post-op CT", "coreg")?;
    let warped_vol = fastwarp_reg::apply::apply_chain(
        &warped_moving,
        &warped_grid,
        &forward,
        Interpolation::Linear,
        0.0,
    )
    .map_err(|e| native_coreg_err(engine, "resampling the post-op CT", &e))?;
    drop(warped_moving);
    let warped_f32: Vec<f32> = warped_vol.data().iter().map(|v| *v as f32).collect();
    drop(warped_vol);
    if data_is_empty(warped_f32.iter().map(|v| f64::from(*v))) {
        return Err(empty_warp("CT"));
    }
    Volume::from_vec(warped_grid, warped_f32)
        .map_err(|e| native_coreg_err(engine, "resampling the post-op CT", &e))?
        .save_nifti(&warped)
        .map_err(|e| native_coreg_err(engine, "writing the warped CT", &e))?;
    merge_record_timings(
        &record_tmp,
        full_s,
        Some(linear_levels(&out.rigid)),
        total_start.elapsed().as_secs_f64(),
    )?;
    Ok(())
}

/// The cohort `Rigid[0.1]` coarse multi-start recipe of `specs/03` §5.1 step 3.
fn coarse_rigid_recipe() -> LinearRecipe {
    LinearRecipe {
        iterations: vec![200, 100],
        shrink_factors: vec![8, 4],
        smoothing_sigmas: vec![3.0, 2.0],
        cache_smoothed_images: false,
        ..LinearRecipe::cohort_rigid()
    }
}

/// The full CT → anchor `Rigid[0.1]` stage (`specs/03` §5.2); memory bounded like the ANTs path.
fn full_coreg_recipe() -> LinearRecipe {
    LinearRecipe {
        cache_smoothed_images: false,
        ..LinearRecipe::cohort_rigid()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_coregister_ants(
    ants: &Path,
    reg: &RegChoice,
    moving: &Path,
    fixed: &Path,
    paths: &CtPaths,
    staged: &Staged,
    tmp_prefix: &Path,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    let total_start = Instant::now();
    let record_tmp = staged.tmp(&paths.record);
    let mut coarse = |start: &StartPaths, _pitch: f64, _c_f: [f64; 3], _c_m: [f64; 3]| {
        let t0 = Instant::now();
        let argv = build_ants_ct_coarse_argv(
            ants,
            moving,
            fixed,
            &start.coarse_prefix,
            &start.start_txt,
            &opts.ants_extra,
        );
        let mut cmd = command_from(&argv, opts);
        let (metric, failure) = run_coarse_start(&mut cmd, &start.coarse_mat, cancel)?;
        Ok(CoarseStart {
            metric,
            seconds: t0.elapsed().as_secs_f64(),
            failure,
        })
    };
    let init = coreg_multistart_init(
        moving,
        fixed,
        StartOutputs {
            work_dir: &paths.work_dir,
            record: &record_tmp,
        },
        reg,
        tx,
        &mut coarse,
    )?;
    log(
        tx,
        format!(
            "[coregister] antsRegistration {} → {}",
            moving.display(),
            fixed.display()
        ),
    );
    let argv = build_ants_ct_coreg_argv(
        ants,
        moving,
        fixed,
        tmp_prefix,
        Some(&init.transform),
        Some(&init.fixed_mask),
        &opts.ants_extra,
    );
    let mut cmd = command_from(&argv, opts);
    let t = Instant::now();
    spawn_logged(&mut cmd, "coregister", cancel, tx)?;
    let full_s = t.elapsed().as_secs_f64();
    let tmp_warped = staged.tmp(&paths.warped);
    if tmp_warped.is_file() && warped_is_empty(&tmp_warped)? {
        return Err(empty_warp("CT"));
    }
    merge_record_timings(
        &record_tmp,
        full_s,
        None,
        total_start.elapsed().as_secs_f64(),
    )?;
    Ok(())
}

pub(crate) fn load_reg_volume(path: &Path, what: &str, stage: &str) -> PipelineResult<Volume<f64>> {
    require_3d(path, stage)?;
    let vol = Volume::<f64>::load_nifti_f64(path).map_err(|e| {
        PipelineError::Message(format!(
            "{stage}: cannot read the {what} {}: {e}",
            path.display()
        ))
    })?;
    Ok(replace_non_finite(vol))
}

pub(crate) fn load_reg_volume_pair(
    first: (&Path, &str),
    second: (&Path, &str),
    stage: &str,
) -> PipelineResult<(Volume<f64>, Volume<f64>)> {
    let (a, b) = rayon::join(
        || load_reg_volume(first.0, first.1, stage),
        || load_reg_volume(second.0, second.1, stage),
    );
    Ok((a?, b?))
}

fn replace_non_finite(vol: Volume<f64>) -> Volume<f64> {
    if vol.data().iter().all(|v| v.is_finite()) {
        return vol;
    }
    let data: Vec<f64> = vol
        .data()
        .iter()
        .map(|v| if v.is_finite() { *v } else { 0.0 })
        .collect();
    Volume::from_vec(*vol.grid(), data).unwrap_or(vol)
}

fn read_affine(path: &Path) -> PipelineResult<AffineTransform> {
    let chain = read_transform_file(path).map_err(|e| {
        PipelineError::Message(format!(
            "coreg: cannot read the coarse start transform {}: {e}",
            path.display()
        ))
    })?;
    match chain.queue().first() {
        Some(TransformElem::Affine(a)) => Ok(*a),
        _ => Err(PipelineError::Message(format!(
            "coreg: {} holds no affine transform",
            path.display()
        ))),
    }
}

fn native_coreg_err(
    engine: &dyn RegEngine,
    what: &str,
    e: &dyn std::fmt::Display,
) -> PipelineError {
    let text = e.to_string();
    let hint = if text.contains("out of memory") || text.contains("binds at most") {
        "; retry with --reg-backend ants"
    } else {
        ""
    };
    PipelineError::Message(format!(
        "coreg: {what} failed on {}: {text}{hint}",
        engine.label()
    ))
}

fn linear_levels(stage: &LinearStageResult) -> Value {
    json!(stage
        .levels
        .iter()
        .map(|l| json!({
            "level": l.level,
            "grid": l.grid_size,
            "samples": l.samples,
            "iterations": l.cg.iterations,
            "converged": l.cg.converged,
            "best_value": l.cg.best_value,
            "seconds": l.seconds,
        }))
        .collect::<Vec<_>>())
}

fn merge_record_timings(
    record: &Path,
    full_s: f64,
    full_levels: Option<Value>,
    elapsed_s: f64,
) -> PipelineResult<()> {
    let Ok(text) = std::fs::read_to_string(record) else {
        return Ok(());
    };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };
    let Some(obj) = v.as_object_mut() else {
        return Ok(());
    };
    let timings = obj.entry("reg_timings").or_insert_with(|| json!({}));
    if let Some(t) = timings.as_object_mut() {
        if let Some(levels) = full_levels {
            t.insert("full_levels".into(), levels);
        }
        t.insert("full_s".into(), json!(full_s));
        t.insert("total_s".into(), json!(elapsed_s));
    }
    let text = serde_json::to_string_pretty(&v)?;
    std::fs::write(record, text + "\n")?;
    Ok(())
}

/// The coreg lock rule of `--reg-backend` (`specs/04` §5.4): `auto` accepts either backend's
/// approved outputs; an explicit request re-runs when the recorded backend differs; a record
/// without the key counts as `ants`.
pub fn coreg_approved_outputs_match(sub: &Subject, opts: &RunOptions) -> Result<(), String> {
    let asked = opts.backend_for(crate::stages::Stage::Coregister);
    if asked == RegBackend::Auto {
        return Ok(());
    }
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    };
    let mut made: Vec<String> = Vec::new();
    if let Some(log) = read(&derivatives::coreg_method_log(sub)) {
        let engine = log.get("engine").and_then(Value::as_str).unwrap_or("ants");
        for entry in log.as_object().into_iter().flat_map(|m| m.values()) {
            if entry.get("image").is_some() {
                made.push(
                    entry
                        .get("backend")
                        .and_then(Value::as_str)
                        .unwrap_or(engine)
                        .to_owned(),
                );
            }
        }
    }
    if made.is_empty() {
        if let Some(rec) = read(&derivatives::coreg_multistart_record(sub)) {
            made.push(
                rec.get("reg_backend")
                    .and_then(Value::as_str)
                    .unwrap_or("ants")
                    .to_owned(),
            );
        }
    }
    match made.into_iter().find(|b| b != asked.as_str()) {
        Some(made_with) => Err(format!(
            "the approved outputs were made with the {made_with} registration backend, this run \
             asks for {}",
            asked.as_str()
        )),
        None => Ok(()),
    }
}

fn data_is_empty(values: impl IntoIterator<Item = f64>) -> bool {
    let mut any_finite = false;
    let mut max_abs = 0.0f64;
    for v in values {
        if v.is_finite() {
            any_finite = true;
            max_abs = max_abs.max(v.abs());
        }
    }
    !any_finite || max_abs < 1e-6
}

fn warped_is_empty(path: &Path) -> PipelineResult<bool> {
    require_3d(path, "the coregistered CT")?;
    let (data, _) = load_volume_f64(path)
        .map_err(|e| PipelineError::Message(format!("coreg: read warped CT: {e}")))?;
    Ok(data_is_empty(data.iter().copied()))
}

fn multistart_failed(e: MultistartError) -> PipelineError {
    PipelineError::Message(format!("coreg multistart: {e}"))
}

/// Coarse rigid starts of `specs/03` §5.1; returns the best start's `0GenericAffine.mat` and
/// the fixed-image mask for the full run.
struct StartOutputs<'a> {
    work_dir: &'a Path,
    record: &'a Path,
}

struct CoarseStart {
    metric: Option<f64>,
    seconds: f64,
    failure: Option<String>,
}

fn coreg_multistart_init<F>(
    moving: &Path,
    fixed: &Path,
    outputs: StartOutputs<'_>,
    reg: &RegChoice,
    tx: Option<&mpsc::Sender<RunEvent>>,
    coarse: &mut F,
) -> PipelineResult<MultistartInit>
where
    F: FnMut(&StartPaths, f64, [f64; 3], [f64; 3]) -> PipelineResult<CoarseStart>,
{
    let StartOutputs {
        work_dir,
        record: record_path,
    } = outputs;
    let (fdat, faff, fhdr) = load_volume_with_header(fixed).map_err(multistart_failed)?;
    let c_f = fixed_head_centroid_lps(&fdat, &faff).map_err(multistart_failed)?;
    let fmask = fixed_registration_mask(&fdat).map_err(multistart_failed)?;
    drop(fdat);
    let (mdat, maff) = load_volume(moving).map_err(multistart_failed)?;
    let c_m = ct_head_centroid_lps(&mdat, &maff).map_err(multistart_failed)?;
    drop(mdat);
    log(
        tx,
        format!(
            "[coregister] multi-start: head centroids (LPS mm) anchor [{:.1}, {:.1}, {:.1}], \
             CT [{:.1}, {:.1}, {:.1}]",
            c_f[0], c_f[1], c_f[2], c_m[0], c_m[1], c_m[2]
        ),
    );

    std::fs::create_dir_all(work_dir)?;
    let mask_path = fixed_mask_path(work_dir);
    unlink_outputs(std::slice::from_ref(&mask_path))?;
    write_fixed_mask(&mask_path, &fmask, &fhdr).map_err(multistart_failed)?;
    drop(fmask);
    let mut scores: Vec<Option<f64>> = Vec::with_capacity(PITCH_STARTS_DEG.len());
    let mut mats: Vec<PathBuf> = Vec::with_capacity(PITCH_STARTS_DEG.len());
    let mut coarse_timings: Vec<Value> = Vec::with_capacity(PITCH_STARTS_DEG.len());
    let mut last_failure: Option<String> = None;
    for &pitch in PITCH_STARTS_DEG.iter() {
        let paths = multistart_work_paths(work_dir, pitch);
        unlink_outputs(&[paths.start_txt.clone(), paths.coarse_mat.clone()])?;
        write_itk_affine_text(&paths.start_txt, &pitch_rotation_lps(pitch), c_f, c_m)?;
        let start = coarse(&paths, pitch, c_f, c_m)?;
        match (start.metric, start.failure.as_deref()) {
            (Some(m), _) => log(
                tx,
                format!("[coregister] start pitch {pitch:+.0}°: metric {m:.4}"),
            ),
            (None, Some(why)) => log(
                tx,
                format!("[coregister] start pitch {pitch:+.0}°: failed: {why}"),
            ),
            (None, None) => log(
                tx,
                format!("[coregister] start pitch {pitch:+.0}°: failed (no metric)"),
            ),
        }
        if start.failure.is_some() {
            last_failure = start.failure;
        }
        scores.push(start.metric);
        coarse_timings.push(json!({"pitch_deg": pitch, "seconds": start.seconds}));
        mats.push(paths.coarse_mat);
    }

    let best = select_best_start(&scores).map_err(|e| match (&e, &last_failure) {
        (MultistartError::AllStartsFailed, Some(why)) => {
            PipelineError::Message(format!("coreg multistart: {e}; last failure: {why}"))
        }
        _ => multistart_failed(e),
    })?;
    let mut record = multistart_record(
        &PITCH_STARTS_DEG,
        &scores,
        best,
        c_f,
        c_m,
        &mask_path,
        reg.resolution.as_str(),
    );
    record.reg_backend = Some(reg.resolution.as_str().to_owned());
    record.reg_requested = Some(reg.requested.as_str().to_owned());
    if let Resolution::Ants { reason: Some(r) } = &reg.resolution {
        record.reg_note = Some(r.clone());
    }
    if let Some(engine) = reg.engine.as_deref() {
        record.reg_adapter = Some(engine.describe());
    }
    record.reg_timings = Some(json!({"coarse": coarse_timings}));
    write_multistart_record(record_path, &record)?;
    log(
        tx,
        format!(
            "[coregister] multi-start: best start pitch {:+.0}°",
            PITCH_STARTS_DEG[best]
        ),
    );
    Ok(MultistartInit {
        transform: mats[best].clone(),
        fixed_mask: mask_path,
    })
}

fn read_tail<R: std::io::Read>(reader: R) -> String {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut tail = String::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                tail.push_str(&String::from_utf8_lossy(&buf));
                if tail.len() > 4 * STDERR_TAIL_CHARS {
                    let cut = tail.len() - 2 * STDERR_TAIL_CHARS;
                    let cut = (cut..tail.len())
                        .find(|&i| tail.is_char_boundary(i))
                        .unwrap_or(0);
                    tail.drain(..cut);
                }
            }
        }
    }
    tail
}

const FAILURE_REASON_CHARS: usize = 300;

fn short_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines[lines.len().saturating_sub(3)..].join(" | ");
    if joined.len() <= FAILURE_REASON_CHARS {
        return joined;
    }
    let start = joined.len() - FAILURE_REASON_CHARS;
    let start = (start..joined.len())
        .find(|&i| joined.is_char_boundary(i))
        .unwrap_or(0);
    format!("…{}", &joined[start..])
}

/// Run one coarse start with stdout captured; the metric is `None` when it fails or reports no
/// metric, with the reason as the second value.
fn run_coarse_start(
    cmd: &mut Command,
    coarse_mat: &Path,
    cancel: Option<&AtomicBool>,
) -> PipelineResult<(Option<f64>, Option<String>)> {
    let _ = std::fs::remove_file(coarse_mat);
    let (mut shortened, _aliases) = crate::outputs::shorten_long_inputs(cmd);
    let cmd = &mut shortened;
    let tool = tool_name(cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::outputs::hide_console_window(cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Ok((None, Some(format!("{tool}: {e}")))),
    };
    let out_handle = child.stdout.take().map(|stdout| {
        thread::spawn(move || {
            let mut tracker = DiagnosticTracker::default();
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => tracker.push_line(&String::from_utf8_lossy(&buf)),
                }
            }
            tracker.finish()
        })
    });
    let err_handle = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_tail(stderr)));
    let join = |out: Option<thread::JoinHandle<Option<f64>>>,
                err: Option<thread::JoinHandle<String>>| {
        let metric = out.and_then(|h| h.join().ok()).flatten();
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        (metric, stderr)
    };

    let status = loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join(out_handle, err_handle);
            return Err(PipelineError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join(out_handle, err_handle);
                return Err(PipelineError::Io(e.to_string()));
            }
        }
    };
    let (metric, stderr) = join(out_handle, err_handle);
    if !status.success() {
        let why = short_reason(&stderr);
        let code = exit_code(&status);
        return Ok((
            None,
            Some(if why.is_empty() {
                format!("{tool} exited with {code}")
            } else {
                format!("{tool} exited with {code}: {why}")
            }),
        ));
    }
    if !coarse_mat.is_file() {
        return Ok((
            None,
            Some(format!("{tool} wrote no {}", file_name_of(coarse_mat))),
        ));
    }
    match metric {
        Some(m) => Ok((Some(m), None)),
        None => Ok((None, Some(format!("{tool} reported no metric")))),
    }
}

fn oom_hint(tool: &str) -> String {
    if tool == "antsApplyTransforms" {
        "antsApplyTransforms killed (signal 9, often OOM). Same mitigations as \
         antsRegistration: coarser --norm-isotropic-mm, etc."
            .to_owned()
    } else {
        "antsRegistration was killed (signal 9, often OOM). Try: --norm-isotropic-mm 1.5 (or \
         2.0) to shrink voxels, dbsloc run --ants-float 1 (or --ants-max-threads 1 for ITK \
         threads), set DBSLOC_ANTS_MAX_THREADS=1, add swap, or a smaller --template. Coarser \
         isotropic spacing runs faster and uses less RAM."
            .to_owned()
    }
}

fn with_oom_hint(e: PipelineError) -> PipelineError {
    match e {
        PipelineError::ToolFailed {
            ref tool, code: -9, ..
        } => PipelineError::Message(oom_hint(tool)),
        other => other,
    }
}

pub const NORM_T2_CHANNEL: &str = "T2w";

#[derive(Debug, Clone, PartialEq)]
pub struct NormT2 {
    pub source: PathBuf,
    pub template: PathBuf,
    pub transform: PathBuf,
    pub warped: PathBuf,
    pub work_prefix: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NormChannels {
    Multispectral(NormT2),
    T1Only(String),
}

pub fn plan_norm_channels(sub: &Subject, opts: &RunOptions) -> PipelineResult<NormChannels> {
    if !opts.norm_multispectral {
        return Ok(NormChannels::T1Only(
            "multispectral normalization disabled (--no-norm-multispectral)".into(),
        ));
    }
    let Some(source) = sub.preop_t2_nonanchor().filter(|p| p.is_file()) else {
        return Ok(NormChannels::T1Only(format!(
            "no pre-op {NORM_T2_CHANNEL} besides the {} anchor",
            sub.anchor_modality
        )));
    };
    let Some(template) = opts.normalization_template_t2.as_ref() else {
        return Ok(NormChannels::T1Only(format!(
            "no T2 template (pass --template-t2, set {}, or place t2.nii[.gz] beside the T1 \
             template)",
            crate::templates::TEMPLATE_T2_ENV
        )));
    };
    if !template.is_file() {
        return Err(PipelineError::Message(format!(
            "normalize: {}",
            crate::templates::missing_template_t2_message(template)
        )));
    }
    Ok(NormChannels::Multispectral(NormT2 {
        source: source.to_path_buf(),
        template: template.clone(),
        transform: derivatives::coreg_t2_affine(sub),
        warped: derivatives::coreg_t2_image(sub),
        work_prefix: coreg_work_prefix(sub, NORM_T2_CHANNEL),
    }))
}

pub(crate) struct NormTools<'a> {
    pub ants: Option<PathBuf>,
    pub apply: Option<PathBuf>,
    pub reg: RegChoice,
    pub cancel: Option<&'a AtomicBool>,
}

type ToolExec<'a> = dyn FnMut(&[String]) -> PipelineResult<()> + 'a;

/// SyN anchor → template, then apply the composite to the post-op CT.
///
/// `RunOptions::norm_isotropic_mm` coarsens the **registration pair only**; the transform is
/// still applied to the CT on the original template grid, because a coarse output grid blurs
/// the electrode artifact below what reconstruction can use (roadmap 1.5).
pub fn run_normalize(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    validate_norm_mm(opts)?;
    NormMethod::parse(&opts.norm_method)
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
    let mut reg =
        crate::reg_backend::choose_for_run(opts.backend_for(crate::stages::Stage::Normalize))
            .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
    if reg.engine.is_some() {
        let channels = norm_channel_count(sub, opts);
        let caps = reg.engine.as_deref().and_then(|e| e.capacity_caps());
        let grid = crate::norm_grid::resolve(opts, channels, caps.as_ref());
        let template = opts
            .normalization_template
            .as_deref()
            .filter(|t| t.is_file());
        if let Some(dims) = template.and_then(|t| finest_grid_of(t, grid.mm)) {
            match crate::reg_backend::capacity_gate(reg.requested, dims, channels, caps.as_ref()) {
                Ok(Some(note)) => {
                    log(tx, format!("dbsloc: note: normalize: {note}"));
                    reg = RegChoice {
                        requested: reg.requested,
                        resolution: Resolution::Ants { reason: Some(note) },
                        engine: None,
                    };
                }
                Ok(None) => {}
                Err(e) => return Err(PipelineError::Message(format!("normalize: {e}"))),
            }
        }
    }
    let uses_ants = reg.engine.is_none();
    let tools = NormTools {
        ants: if uses_ants {
            Some(require_binary("antsRegistration", "normalize")?)
        } else {
            None
        },
        apply: if has_coregistered_postop(sub) && uses_ants {
            Some(require_binary("antsApplyTransforms", "normalize")?)
        } else {
            None
        },
        reg,
        cancel,
    };
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, "normalize", cancel, tx)
    };
    let outcome = normalize_with(sub, opts, &tools, &mut exec, tx)?;
    if let Some(template) = opts.normalization_template.as_deref() {
        match crate::space_bridge::ensure_bridge(template, opts, cancel, tx) {
            Ok(_) => {}
            Err(PipelineError::Cancelled) => log(
                tx,
                "dbsloc: warning: bridge: cancelled; reconstruct builds it when it needs it"
                    .to_owned(),
            ),
            Err(e) => log(
                tx,
                format!(
                    "dbsloc: warning: bridge: {e}; reconstruction will lack reference-space coordinates"
                ),
            ),
        }
    }
    Ok(outcome)
}

fn has_coregistered_postop(sub: &Subject) -> bool {
    sub.has_postop_ct()
        || sub
            .postop_mr_images()
            .iter()
            .any(|(key, _)| sub.coreg.anat_postop.contains_key(key))
}

struct NormApply {
    key: String,
    src: PathBuf,
    out: PathBuf,
}

fn norm_apply_items(sub: &Subject, space: &str) -> PipelineResult<Vec<NormApply>> {
    let mut items = Vec::new();
    if sub.has_postop_ct() {
        let src = sub
            .coreg
            .anat_postop
            .get(derivatives::CT_KEY)
            .cloned()
            .ok_or_else(|| {
                PipelineError::Message(
                    "no coregistered CT (*_space-anchorNative_desc-preproc_CT.nii.gz); run coreg \
                     or check CT/anchor"
                        .into(),
                )
            })?;
        items.push(NormApply {
            key: derivatives::CT_KEY.into(),
            src,
            out: derivatives::norm_postop_image(sub, derivatives::CT_KEY, space),
        });
    }
    for (key, _) in sub.postop_mr_images() {
        if let Some(src) = sub.coreg.anat_postop.get(key).cloned() {
            items.push(NormApply {
                key: key.to_owned(),
                src,
                out: derivatives::norm_postop_image(sub, key, space),
            });
        }
    }
    Ok(items)
}

/// The finest registration grid of `normalize`: the template resampled to `iso_mm` (the pipeline
/// rule), or the template grid itself for the full resolution. Reads only the NIfTI header;
/// `None` when the template cannot be read (the stage reports that later).
pub(crate) fn finest_grid_of(template: &Path, iso_mm: Option<f64>) -> Option<[usize; 3]> {
    let grid = fastwarp_reg::nifti_io::read_grid(template).ok()?;
    Some(match iso_mm {
        Some(mm) => crate::resample::resampled_grid(grid.size(), grid.spacing(), mm).0,
        None => grid.size(),
    })
}

/// Number of SyN channels this subject will use (2 = anchor + T2w), without planning errors.
pub(crate) fn norm_channel_count(sub: &Subject, opts: &RunOptions) -> usize {
    let multispectral = opts.norm_multispectral
        && sub.preop_t2_nonanchor().filter(|p| p.is_file()).is_some()
        && opts
            .normalization_template_t2
            .as_ref()
            .is_some_and(|t| t.is_file());
    if multispectral {
        2
    } else {
        1
    }
}

fn validate_norm_mm(opts: &RunOptions) -> PipelineResult<()> {
    if let NormGrid::Mm(mm) = opts.norm_isotropic_mm {
        if mm <= 0.0 || !mm.is_finite() {
            return Err(PipelineError::Message(
                "norm isotropic resample size must be > 0".into(),
            ));
        }
    }
    Ok(())
}

pub(crate) type AntsPair = (PathBuf, PathBuf, Option<(PathBuf, PathBuf)>);

pub(crate) fn ants_registration_inputs(
    stage: &str,
    work: &Path,
    iso_mm: Option<f64>,
    moving: &Path,
    template: &Path,
    t2_moving: Option<(PathBuf, PathBuf)>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<AntsPair> {
    let Some(mm) = iso_mm else {
        return Ok((moving.to_path_buf(), template.to_path_buf(), t2_moving));
    };
    let f = py_float(mm);
    log(
        tx,
        format!("[{stage}] resampling registration pair to {f} mm isotropic"),
    );
    let m_out = work.join(format!("moving_is{f}mm.nii.gz"));
    let t_out = work.join(format!("template_is{f}mm.nii.gz"));
    let m2_out = work.join(format!("moving2_is{f}mm.nii.gz"));
    let t2_out = work.join(format!("template2_is{f}mm.nii.gz"));
    unlink_outputs(&[m_out.clone(), t_out.clone(), m2_out.clone(), t2_out.clone()])?;
    let m = resample_for_ants(moving, &m_out, mm)?;
    let t = resample_for_ants(template, &t_out, mm)?;
    let pair2 = match &t2_moving {
        Some((m2, t2)) => Some((
            resample_for_ants(m2, &m2_out, mm)?,
            resample_for_ants(t2, &t2_out, mm)?,
        )),
        None => None,
    };
    Ok((m, t, pair2))
}

fn resample_for_ants(src: &Path, out: &Path, mm: f64) -> PipelineResult<PathBuf> {
    crate::resample::resample_isotropic(src, out, mm)
        .map_err(|e| PipelineError::Message(format!("normalize: resample for ANTs: {e}")))
}

fn path_json(p: &Path) -> Value {
    json!(absolute(p).to_string_lossy())
}

fn norm_channels_json(
    sub: &Subject,
    moving: &Path,
    template: &Path,
    channels: &NormChannels,
    grid: &ResolvedNormGrid,
    reg: Option<&Map<String, Value>>,
) -> Value {
    let engine = reg
        .and_then(|r| r.get("reg_backend"))
        .and_then(Value::as_str)
        .unwrap_or("ants");
    let mut m = method_log_base(NORM_METHOD_ANTS_SYN, engine);
    match channels {
        NormChannels::Multispectral(t2) => {
            m.insert(
                "channels".into(),
                json!([sub.anchor_modality, NORM_T2_CHANNEL]),
            );
            m.insert(
                "moving".into(),
                json!([path_json(moving), path_json(&t2.warped)]),
            );
            m.insert(
                "template".into(),
                json!([path_json(template), path_json(&t2.template)]),
            );
            m.insert(
                "syn_cc_weights".into(),
                json!([
                    crate::ants_argv::MULTISPECTRAL_CC_WEIGHT,
                    crate::ants_argv::MULTISPECTRAL_CC_WEIGHT
                ]),
            );
            m.insert("t2_source".into(), path_json(&t2.source));
            m.insert("t2_coreg_transform".into(), path_json(&t2.transform));
            m.insert("t2_coreg_warped".into(), path_json(&t2.warped));
        }
        NormChannels::T1Only(reason) => {
            m.insert("channels".into(), json!([sub.anchor_modality]));
            m.insert("moving".into(), json!([path_json(moving)]));
            m.insert("template".into(), json!([path_json(template)]));
            m.insert("syn_cc_weights".into(), json!(["1"]));
            m.insert("t2_source".into(), Value::Null);
            m.insert("t2_coreg_transform".into(), Value::Null);
            m.insert("t2_coreg_warped".into(), Value::Null);
            m.insert("note".into(), json!(reason));
        }
    }
    m.insert("norm_isotropic_mm".into(), json!(grid.mm));
    m.insert("norm_grid_source".into(), json!(grid.source.as_str()));
    m.insert("norm_grid_note".into(), json!(grid.note));
    m.insert(
        "template_space".into(),
        json!(crate::template_catalog::infer_space(template)),
    );
    if let Some(reg) = reg {
        for (k, v) in reg {
            m.insert(k.clone(), v.clone());
        }
    }
    Value::Object(m)
}

fn reg_record(reg: &RegChoice, extra: Option<Map<String, Value>>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("reg_backend".into(), json!(reg.resolution.as_str()));
    m.insert("reg_requested".into(), json!(reg.requested.as_str()));
    if let Resolution::Ants {
        reason: Some(reason),
    } = &reg.resolution
    {
        m.insert("reg_note".into(), json!(reason));
    }
    if let Some(engine) = reg.engine.as_deref() {
        m.insert("reg_adapter".into(), engine.describe());
    }
    if let Some(extra) = extra {
        m.extend(extra);
    }
    m
}

pub fn read_norm_channels(sub: &Subject) -> Option<Value> {
    let text = std::fs::read_to_string(derivatives::norm_method_log(sub)).ok()?;
    serde_json::from_str(&text).ok()
}

fn grid_text(grid: Option<f64>) -> String {
    grid.map_or_else(
        || "the full template grid".to_owned(),
        |mm| format!("a {} mm grid", py_float(mm)),
    )
}

pub(crate) fn recorded_template(rec: &Value, index: usize) -> Option<PathBuf> {
    rec.get("template")?
        .get(index)?
        .as_str()
        .map(|s| absolute(Path::new(s)))
}

pub fn approved_outputs_match(sub: &Subject, opts: &RunOptions) -> Result<(), String> {
    NormMethod::parse(&opts.norm_method)?;
    let Some(template) = opts.normalization_template.as_deref() else {
        return Ok(());
    };
    let record = read_norm_channels(sub);
    let have = record
        .as_ref()
        .and_then(|r| r.get("method"))
        .and_then(Value::as_str)
        .unwrap_or(NORM_METHOD_ANTS_SYN);
    if have != NORM_METHOD_ANTS_SYN {
        return Err(format!(
            "the approved outputs were made with {have}, this run asks for {NORM_METHOD_ANTS_SYN}"
        ));
    }
    if opts.backend_for(crate::stages::Stage::Normalize) != RegBackend::Auto {
        if let Some(rec) = record.as_ref() {
            let made_with = rec
                .get("reg_backend")
                .and_then(Value::as_str)
                .unwrap_or("ants");
            if made_with != opts.backend_for(crate::stages::Stage::Normalize).as_str() {
                return Err(format!(
                    "the approved outputs were made with the {made_with} registration backend, \
                     this run asks for {}",
                    opts.backend_for(crate::stages::Stage::Normalize).as_str()
                ));
            }
        }
    }
    let Some(rec) = record else {
        return Ok(());
    };
    let asked_grid = match opts.norm_isotropic_mm {
        NormGrid::Template => None,
        NormGrid::Mm(mm) => Some(Some(mm)),
        NormGrid::Full => Some(None),
    };
    if let (Some(value), Some(want)) = (rec.get("norm_isotropic_mm"), asked_grid) {
        let have_grid = value.as_f64();
        let same = match (have_grid, want) {
            (None, None) => value.is_null(),
            (Some(a), Some(b)) => (a - b).abs() < 1e-6,
            _ => false,
        };
        if !same && (have_grid.is_some() || value.is_null()) {
            return Err(format!(
                "the approved outputs were made on {}, this run asks for {}",
                grid_text(have_grid),
                grid_text(want)
            ));
        }
    }
    if let Some(have_template) = recorded_template(&rec, 0) {
        if have_template != absolute(template) {
            return Err(format!(
                "the approved outputs were made for the template {}, this run asks for {}",
                have_template.display(),
                absolute(template).display()
            ));
        }
    }
    if let Some(have_space) = rec.get("template_space").and_then(Value::as_str) {
        let want_space = crate::template_catalog::infer_space(template);
        if have_space != want_space {
            return Err(format!(
                "the approved outputs were made in the template space {have_space}, this run asks for {want_space}"
            ));
        }
    }
    if let (Some(channels), Ok(planned)) = (
        rec.get("channels").and_then(Value::as_array),
        plan_norm_channels(sub, opts),
    ) {
        let had_t2 = channels.len() > 1;
        match (&planned, had_t2) {
            (NormChannels::Multispectral(t2), true) => {
                if let Some(have_t2) = recorded_template(&rec, 1) {
                    if have_t2 != absolute(&t2.template) {
                        return Err(format!(
                            "the approved outputs were made with the T2 template {}, this run asks for {}",
                            have_t2.display(),
                            absolute(&t2.template).display()
                        ));
                    }
                }
            }
            (NormChannels::Multispectral(_), false) => {
                return Err(
                    "the approved outputs were made from the anchor image only, this run asks for a T1w + T2w normalization"
                        .into(),
                );
            }
            (NormChannels::T1Only(_), true) => {
                return Err(
                    "the approved outputs were made from T1w + T2w, this run asks for the anchor image only"
                        .into(),
                );
            }
            (NormChannels::T1Only(_), false) => {}
        }
    }
    Ok(())
}

struct NormTransformPaths<'a> {
    forward: &'a Path,
    inverse: &'a Path,
    forward_affine: &'a Path,
    inverse_affine: &'a Path,
}

/// Writes the linear part of the forward composite and its inverse into the staged set and
/// records them in the method log. `affine_native_to_template_4x4` maps RAS mm native points to
/// RAS mm template points (the inverse `.mat`, converted from LPS).
fn stage_norm_affines(
    staged: &Staged,
    paths: &NormTransformPaths<'_>,
    forward: &fastwarp_reg::transform::AffineTransform,
    rec: &mut Value,
) -> PipelineResult<()> {
    let inverse = crate::norm_native::write_norm_affines(
        forward,
        &staged.tmp(paths.forward_affine),
        &staged.tmp(paths.inverse_affine),
    )?;
    if let Some(m) = rec.as_object_mut() {
        m.insert(
            "transforms".into(),
            json!({
                "forward": path_json(paths.forward),
                "inverse": path_json(paths.inverse),
                "forward_affine": path_json(paths.forward_affine),
                "inverse_affine": path_json(paths.inverse_affine),
            }),
        );
        m.insert(
            "affine_native_to_template_4x4".into(),
            json!(crate::norm_native::ras_4x4(&inverse)),
        );
    }
    Ok(())
}

/// Writes the two normalization `.mat` files from the forward composite of a normalization that
/// predates them; the method log is left as it is. Returns the files written.
pub fn backfill_norm_affines(sub: &Subject) -> Result<Vec<PathBuf>, String> {
    let Some(space) = sub.norm.template_space.as_deref() else {
        return Ok(Vec::new());
    };
    let composite = derivatives::norm_forward(sub, space);
    let forward_affine = derivatives::norm_forward_affine(sub, space);
    let inverse_affine = derivatives::norm_inverse_affine(sub, space);
    if !composite.is_file() || (forward_affine.is_file() && inverse_affine.is_file()) {
        return Ok(Vec::new());
    }
    let linear =
        crate::norm_native::read_forward_linear_part(&composite).map_err(|e| e.to_string())?;
    let staged = Staged::new(vec![forward_affine.clone(), inverse_affine.clone()])
        .map_err(|e| e.to_string())?;
    let res = crate::norm_native::write_norm_affines(
        &linear,
        &staged.tmp(&forward_affine),
        &staged.tmp(&inverse_affine),
    );
    staged.finish(res, None).map_err(|e| e.to_string())?;
    Ok(vec![forward_affine, inverse_affine])
}

pub(crate) fn normalize_with(
    sub: &Subject,
    opts: &RunOptions,
    tools: &NormTools<'_>,
    exec: &mut ToolExec<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    validate_norm_mm(opts)?;
    let method = NormMethod::parse(&opts.norm_method)
        .map_err(|e| PipelineError::Message(format!("normalize: {e}")))?;
    let native = tools.reg.engine.as_deref();
    let template = opts
        .normalization_template
        .as_ref()
        .filter(|t| t.is_file())
        .ok_or_else(|| {
            PipelineError::Message(format!(
                "normalize: {}",
                crate::templates::missing_template_message(
                    opts.normalization_template.as_deref(),
                    None
                )
            ))
        })?;
    let moving_buf = sub
        .anchor_in_use()
        .filter(|p| p.is_file())
        .ok_or_else(|| PipelineError::Message("no anchor moving image".into()))?;
    let moving = moving_buf.as_path();
    let space = crate::template_catalog::infer_space(template);
    let items = norm_apply_items(sub, &space)?;
    let apply_bin = if items.is_empty() || native.is_some() {
        None
    } else {
        Some(tools.apply.as_deref().ok_or_else(|| {
            PipelineError::Message(
                "normalize: antsApplyTransforms not found (bundled or PATH)".into(),
            )
        })?)
    };
    let channels = plan_norm_channels(sub, opts)?;
    if let Resolution::Ants { reason: Some(r) } = &tools.reg.resolution {
        log(
            tx,
            format!("dbsloc: note: normalize: registration backend ants: {r}"),
        );
    }
    let caps = native.and_then(|e| e.capacity_caps());
    let grid = crate::norm_grid::resolve(opts, norm_channel_count(sub, opts), caps.as_ref());
    if let Some(note) = &grid.note {
        log(
            tx,
            format!(
                "dbsloc: warning: normalize: {note}; {}",
                crate::norm_grid::STEP_REMEDY
            ),
        );
    }
    if grid.mm.is_some() {
        require_3d(moving, "normalize")?;
        require_3d(template, "normalize")?;
        if let NormChannels::Multispectral(t2) = &channels {
            require_3d(&t2.template, "normalize")?;
        }
    }

    std::fs::create_dir_all(&sub.norm_dir)?;
    std::fs::create_dir_all(derivatives::norm_transform_dir(sub))?;
    std::fs::create_dir_all(derivatives::norm_log_dir(sub))?;
    let tmp_prefix = norm_work_prefix(sub);
    let anchor_out = derivatives::norm_anchor_image(sub, &space);
    let composite = derivatives::norm_forward(sub, &space);
    let inverse_composite = derivatives::norm_inverse(sub, &space);
    let forward_affine = derivatives::norm_forward_affine(sub, &space);
    let inverse_affine = derivatives::norm_inverse_affine(sub, &space);
    let record = derivatives::norm_method_log(sub);
    let mut pairs: Vec<(PathBuf, PathBuf)> = vec![
        (
            anchor_out.clone(),
            suffixed(&tmp_prefix, ANTS_WARPED_SUFFIX),
        ),
        (
            composite.clone(),
            suffixed(&tmp_prefix, ANTS_COMPOSITE_SUFFIX),
        ),
        (
            inverse_composite.clone(),
            suffixed(&tmp_prefix, ANTS_INVERSE_COMPOSITE_SUFFIX),
        ),
        (
            forward_affine.clone(),
            crate::outputs::partial_of(&forward_affine),
        ),
        (
            inverse_affine.clone(),
            crate::outputs::partial_of(&inverse_affine),
        ),
        (record.clone(), crate::outputs::partial_of(&record)),
    ];
    let postop_keys = std::iter::once(derivatives::CT_KEY).chain(crate::bids::POSTOP_MR_KEYS);
    let mut stale_spaces = vec![space.clone()];
    if let Some(old) = sub.norm.template_space.as_deref() {
        if old != space {
            stale_spaces.push(old.to_owned());
        }
    }
    for key in postop_keys {
        for (i, sp) in stale_spaces.iter().enumerate() {
            let out = derivatives::norm_postop_image(sub, key, sp);
            let tmp = if i == 0 {
                postop_work_image(sub, key)
            } else {
                crate::outputs::partial_of(&out)
            };
            pairs.push((out, tmp));
        }
    }
    if stale_spaces.len() > 1 {
        let old = &stale_spaces[1];
        for stale in [
            derivatives::norm_anchor_image(sub, old),
            derivatives::norm_forward(sub, old),
            derivatives::norm_inverse(sub, old),
            derivatives::norm_forward_affine(sub, old),
            derivatives::norm_inverse_affine(sub, old),
        ] {
            pairs.push((stale.clone(), crate::outputs::partial_of(&stale)));
        }
    }
    if let NormChannels::Multispectral(_) = &channels {
        std::fs::create_dir_all(&sub.coreg_dir)?;
        std::fs::create_dir_all(derivatives::coreg_transform_dir(sub))?;
    }
    let t2_prefix = coreg_work_prefix(sub, NORM_T2_CHANNEL);
    pairs.push((
        derivatives::coreg_t2_affine(sub),
        suffixed(&t2_prefix, ANTS_AFFINE_SUFFIX),
    ));
    pairs.push((
        derivatives::coreg_t2_image(sub),
        suffixed(&t2_prefix, ANTS_WARPED_SUFFIX),
    ));
    let staged = Staged::mapped(pairs)?;

    let resample_dir = derivatives::norm_work_dir(sub);
    let transforms = NormTransformPaths {
        forward: &composite,
        inverse: &inverse_composite,
        forward_affine: &forward_affine,
        inverse_affine: &inverse_affine,
    };
    let res = (|| {
        if let Some(engine) = native {
            let (extra, linear) = crate::norm_native::run(
                &crate::norm_native::NativeNormalize {
                    engine,
                    moving,
                    template,
                    channels: &channels,
                    iso_mm: grid.mm,
                    threads: opts.ants_max_threads,
                    apply: &items
                        .iter()
                        .map(|item| crate::norm_native::NativeApply {
                            key: item.key.clone(),
                            src: item.src.clone(),
                            tmp_out: staged.tmp(&item.out),
                        })
                        .collect::<Vec<_>>(),
                    tmp_prefix: &tmp_prefix,
                    cancel: tools.cancel,
                },
                tx,
            )?;
            let mut rec = norm_channels_json(
                sub,
                moving,
                template,
                &channels,
                &grid,
                Some(&reg_record(&tools.reg, Some(extra))),
            );
            stage_norm_affines(&staged, &transforms, &linear, &mut rec)?;
            let text = serde_json::to_string_pretty(&rec)?;
            std::fs::write(staged.tmp(&record), text + "\n")?;
            return Ok(());
        }
        let ants = tools.ants.as_deref().ok_or_else(|| {
            PipelineError::Message("normalize: antsRegistration not found (bundled or PATH)".into())
        })?;
        let t2_moving = match &channels {
            NormChannels::Multispectral(t2) => {
                log(
                    tx,
                    format!(
                        "[normalize] antsRegistration rigid {} → {} ({NORM_T2_CHANNEL} channel)",
                        t2.source.display(),
                        moving.display()
                    ),
                );
                let argv = build_ants_mr_coreg_argv(
                    ants,
                    &t2.source,
                    moving,
                    &t2.work_prefix,
                    &opts.ants_extra,
                );
                exec(&argv).map_err(with_oom_hint)?;
                let warped = staged.tmp(&t2.warped);
                if !warped.is_file() {
                    return Err(PipelineError::Message(format!(
                        "normalize: {NORM_T2_CHANNEL} coregistration wrote no {}",
                        file_name_of(&t2.warped)
                    )));
                }
                Some((warped, t2.template.clone()))
            }
            NormChannels::T1Only(reason) => {
                log(
                    tx,
                    format!(
                        "dbsloc: note: normalize: {} channel only: {reason}",
                        sub.anchor_modality
                    ),
                );
                None
            }
        };
        let engine = "antsRegistration SyN";
        log(
            tx,
            format!(
                "[normalize] {engine} {} → template ({})",
                moving.display(),
                if t2_moving.is_some() {
                    format!("{} + {NORM_T2_CHANNEL}", sub.anchor_modality)
                } else {
                    sub.anchor_modality.clone()
                }
            ),
        );
        let (reg_moving, reg_template, reg_t2) = ants_registration_inputs(
            "normalize",
            &resample_dir,
            grid.mm,
            moving,
            template,
            t2_moving,
            tx,
        )?;
        let argv = match &reg_t2 {
            Some((m2, t2)) => build_ants_syn_multispectral_argv(
                ants,
                &reg_moving,
                &reg_template,
                m2,
                t2,
                &tmp_prefix,
                &opts.ants_extra,
            ),
            None => build_ants_syn_argv(
                ants,
                &reg_moving,
                &reg_template,
                &tmp_prefix,
                &opts.ants_extra,
            ),
        };
        exec(&argv).map_err(with_oom_hint)?;
        if let Some(apply_bin) = apply_bin {
            for item in &items {
                apply_postop_to_template(
                    apply_bin,
                    opts,
                    &item.key,
                    &item.src,
                    template,
                    &staged.tmp(&composite),
                    &staged.tmp(&item.out),
                    exec,
                    tx,
                )
                .map_err(with_oom_hint)?;
            }
        }
        let linear = crate::norm_native::read_forward_linear_part(&staged.tmp(&composite))?;
        let mut rec = norm_channels_json(
            sub,
            moving,
            template,
            &channels,
            &grid,
            Some(&reg_record(&tools.reg, None)),
        );
        stage_norm_affines(&staged, &transforms, &linear, &mut rec)?;
        let text = serde_json::to_string_pretty(&rec)?;
        std::fs::write(staged.tmp(&record), text + "\n")?;
        Ok(())
    })();
    let finished = staged.finish(res, Some(&stage_lock(sub, "normalize")));
    let _ = std::fs::remove_dir_all(&resample_dir);
    finished?;

    log(
        tx,
        format!("[normalize] done: {}", file_name_of(&anchor_out)),
    );
    Ok(StageOutcome::done(format!(
        "ok:normalization:{}:{}",
        method.as_str(),
        image_token(&anchor_out)
    )))
}

#[allow(clippy::too_many_arguments)]
fn apply_postop_to_template(
    apply_bin: &Path,
    opts: &RunOptions,
    key: &str,
    ct_src: &Path,
    template: &Path,
    composite: &Path,
    out: &Path,
    exec: &mut ToolExec<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<()> {
    if !composite.is_file() {
        return Err(PipelineError::Message(format!(
            "missing SyN composite {} (antsRegistration output)",
            file_name_of(composite).trim_start_matches(crate::outputs::PARTIAL_PREFIX)
        )));
    }
    log(
        tx,
        format!(
            "[normalize] applying SyN to {key} → {}",
            file_name_of(out).trim_start_matches(crate::outputs::PARTIAL_PREFIX)
        ),
    );
    let argv = build_ants_apply_transforms_argv(
        apply_bin,
        ct_src,
        template,
        out,
        composite,
        &opts.ants_extra,
    );
    exec(&argv)
}

pub const SCRF_STUB_RESULT: &str = "plan:scrf:subprocess_not_implemented";
const SCRF_ENV: [&str; 5] = [
    "DBSLOC_SCRF_MOVING",
    "DBSLOC_SCRF_REFERENCE",
    "DBSLOC_SCRF_OUTPUT",
    "DBSLOC_SCRF_WARP",
    "DBSLOC_SCRF_AFFINE",
];

pub fn run_brainshift(
    sub: &Subject,
    opts: &RunOptions,
    review: Option<&crate::checkreg::ReviewSetup>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let on = std::env::var("DBSLOC_SCRF_ANTS")
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !on {
        return Ok(StageOutcome::skipped_as(
            "brainshift: subcortical refinement not implemented (specs/04); DBSLOC_SCRF_ANTS=1 \
             applies an existing transform",
            SCRF_STUB_RESULT,
        ));
    }
    let vals: Vec<String> = SCRF_ENV
        .iter()
        .map(|k| std::env::var(k).unwrap_or_default().trim().to_owned())
        .collect();
    if vals.iter().any(String::is_empty) {
        return Err(PipelineError::Message(
            "scrf: DBSLOC_SCRF_ANTS=1 requires DBSLOC_SCRF_MOVING, DBSLOC_SCRF_REFERENCE, \
             DBSLOC_SCRF_OUTPUT, DBSLOC_SCRF_WARP, DBSLOC_SCRF_AFFINE"
                .into(),
        ));
    }
    let [mov, refe, out, warp, aff] = [0, 1, 2, 3, 4].map(|i| PathBuf::from(&vals[i]));
    let exe = find_binary("antsApplyTransforms").ok_or_else(|| {
        PipelineError::Message("antsApplyTransforms: binary not found (bundled or PATH)".into())
    })?;
    for t in [&warp, &aff] {
        if !t.is_file() {
            return Err(PipelineError::Message(format!(
                "antsApplyTransforms: missing {}",
                t.display()
            )));
        }
    }
    if !mov.is_file() || !refe.is_file() {
        return Err(PipelineError::Message(
            "antsApplyTransforms: moving or reference file missing".into(),
        ));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged = Staged::new(vec![out.clone()])?;
    let mut cmd = Command::new(exe);
    cmd.args(["-d", "3", "-i"])
        .arg(absolute(&mov))
        .arg("-r")
        .arg(absolute(&refe))
        .arg("-o")
        .arg(absolute(&staged.tmp(&out)))
        .args(["-n", "Linear", "-t"])
        .arg(format!("[{},0]", absolute(&warp).display()))
        .arg("-t")
        .arg(format!("[{},0]", absolute(&aff).display()));
    if let Some(env) = ants_env(opts.ants_max_threads) {
        cmd.envs(env);
    }
    let res = spawn_logged(&mut cmd, "brainshift", cancel, tx).map_err(|e| match e {
        PipelineError::ToolFailed {
            code, stderr_tail, ..
        } => PipelineError::Message(format!("antsApplyTransforms (code {code}): {stderr_tail}")),
        other => other,
    });
    staged.finish(res, Some(&stage_lock(sub, "brainshift")))?;
    let mut mlog = method_log_base("scrf", "ants");
    mlog.insert("moving".into(), path_json(&mov));
    mlog.insert("reference".into(), path_json(&refe));
    mlog.insert("output".into(), path_json(&out));
    mlog.insert("warp".into(), path_json(&warp));
    mlog.insert("affine".into(), path_json(&aff));
    write_method_log(&derivatives::brainshift_method_log(sub), mlog)?;
    if opts.checkreg {
        let bs = BrainshiftPaths {
            anat_moving: mov,
            anat_anchor: refe,
            anat_scrf: Some(out.clone()),
            log_method: derivatives::brainshift_method_log(sub),
        };
        run_brainshift_checkreg(sub, opts, &bs, review, cancel, tx).map_err(PipelineError::from)?;
    }
    Ok(StageOutcome::done(format!(
        "ok:scrf:ants_apply:{}",
        file_name_of(&out)
    )))
}

const NO_ELECTRODE_MSG: &str = "no electrode model set; pass el_model in RunOptions (CLI \
     `--el-model '<name>'`, GUI reconstruct panel) or set elmodel in the subject's \
     prefs/sub-<id>_desc-uiprefs.mat";

fn el_model_error(opts: &RunOptions) -> String {
    opts.el_model_error
        .clone()
        .unwrap_or_else(|| NO_ELECTRODE_MSG.to_owned())
}

/// Resolve `opts.el_model` against the catalogue.
///
/// There is no default. A 1.5 mm-pitch 4-contact lead used to stand in when nothing was set,
/// which matches no real device: 9 of the 10 Lead-Tutor subjects carry 8-contact directional
/// leads, and even the Medtronic rings pitch at 2.0 or 3.0 mm. Contact coordinates and every
/// plausibility tolerance derive from this, so an unresolved model has to stop the stage.
fn resolve_electrode(opts: &RunOptions) -> PipelineResult<&'static ElectrodeGeometry> {
    let name = opts
        .el_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PipelineError::Prereq(el_model_error(opts)))?;
    electrode_by_name(name).ok_or_else(|| {
        PipelineError::Prereq(format!(
            "E_RECON_elmodel: {name:?} is not in the electrode catalogue ({} known)",
            dbsloc_reconstruct::electrode_model_names().len()
        ))
    })
}

pub(crate) fn opts_with_prefs_model(
    sub: &Subject,
    opts: &RunOptions,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Option<RunOptions> {
    if opts
        .el_model
        .as_deref()
        .is_some_and(|m| !m.trim().is_empty())
    {
        return None;
    }
    let model = match crate::uiprefs::subject_electrode_model(sub) {
        Ok(model) => model?,
        Err(e) => {
            log(
                tx,
                format!("dbsloc: warning: reconstruct: cannot read the electrode model from {e}"),
            );
            return None;
        }
    };
    log(
        tx,
        format!(
            "dbsloc: note: reconstruct: electrode model from {}: {model}",
            sub.uiprefs_path().display()
        ),
    );
    let mut resolved = opts.clone();
    resolved.el_model = Some(model);
    Some(resolved)
}

pub fn run_reconstruct(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let resolved = opts_with_prefs_model(sub, opts, tx);
    let opts = resolved.as_ref().unwrap_or(opts);
    let method = opts.recon_method.trim().to_ascii_lowercase();
    if method == "pacer" && !crate::ct_recon::PACER_AVAILABLE {
        return Err(recon_err(PipelineError::Message(
            crate::ct_recon::PACER_UNAVAILABLE.into(),
        )));
    }
    let modality = sub.postop_modality.as_deref();
    if modality != Some("CT") {
        if method == "pacer" && modality == Some("MRI") {
            return Err(recon_err(PipelineError::Message(
                "E_PACER_mri: PaCER is CT-only (spec 05).".into(),
            )));
        }
        if method == "pacer" || method == "trac_core_native" {
            return Err(recon_err(PipelineError::Message(format!(
                "E_RECON: recon_method={method} requires post-op CT (spec 05); the native path \
                 traces the coregistered CT"
            ))));
        }
        let kept = kept_corrections(sub, opts, EditSpace::Mni, tx);
        let skip = kept_keys(kept.as_ref());
        let mut reco = if skip.len() == SIDE_KEYS.len() {
            kept_only(kept.as_ref())
        } else {
            let world_to_reference = trac_world_to_reference(opts, cancel, tx)?;
            reconstruct_template(sub, opts, world_to_reference, tx, &skip).map_err(recon_err)?
        };
        return finish_reconstruction(
            sub,
            opts,
            &mut reco,
            kept.as_ref(),
            EditSpace::Mni,
            cancel,
            tx,
        );
    }
    let ct_method = CtMethod::parse(&method).ok_or_else(|| {
        recon_err(PipelineError::Message(format!(
            "E_RECON: unknown recon_method {method:?} for a post-op CT (auto, trac_core, \
             trac_core_native, pacer)"
        )))
    })?;
    let kept = kept_corrections(sub, opts, EditSpace::Native, tx);
    let skip = kept_keys(kept.as_ref());
    if skip.len() == SIDE_KEYS.len() {
        let mut reco = kept_only(kept.as_ref());
        return finish_reconstruction(
            sub,
            opts,
            &mut reco,
            kept.as_ref(),
            EditSpace::Native,
            cancel,
            tx,
        );
    }
    let template = opts.normalization_template.clone();
    let ct = sub.coreg.anat_postop.get(derivatives::CT_KEY).cloned();
    let warp_route = warp_route_for(sub, opts);
    let world_to_reference = trac_world_to_reference(opts, cancel, tx)?;
    let norm_inverse_composite = normalized_inverse_composite(sub, template.as_deref());
    let brain_masks = if ct_method == CtMethod::Pacer && !derivatives::pacer_import(sub).is_file() {
        crate::ct_mask::gen_ct_mask(sub, opts, norm_inverse_composite.as_deref(), cancel, tx)?
            .map(|m| m.candidates())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let req = CtReconRequest {
        warp_route,
        world_to_reference,
        ct,
        norm_inverse_composite,
        brain_masks,
        template,
        el_model: opts.el_model.clone(),
        el_model_error: Some(el_model_error(opts)),
        metal_dark: !opts.recon_bright_metal,
        ants_threads: opts.ants_max_threads,
        pacer_import: Some(derivatives::pacer_import(sub)),
        skip_sides: skip.iter().map(|k| (*k).to_owned()).collect(),
        ..CtReconRequest::new(sub.subj_id.clone(), ct_method)
    };
    let mut reco = reconstruct_ct(&req, cancel, &mut |line| log(tx, line))
        .map_err(|e: CtReconError| recon_err(PipelineError::Message(e.to_string())))?;
    finish_reconstruction(
        sub,
        opts,
        &mut reco,
        kept.as_ref(),
        EditSpace::Native,
        cancel,
        tx,
    )
}

fn kept_corrections(
    sub: &Subject,
    opts: &RunOptions,
    space: EditSpace,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Option<KeptSides> {
    crate::manual_correction::kept_sides(sub, opts, space, &mut |line| log(tx, line))
}

fn kept_keys(kept: Option<&KeptSides>) -> Vec<&'static str> {
    kept.map(|k| k.keys.clone()).unwrap_or_default()
}

fn kept_only(kept: Option<&KeptSides>) -> Value {
    let mut reco = kept.map(|k| k.old.clone()).unwrap_or(Value::Null);
    if let Some(obj) = reco.as_object_mut() {
        obj.remove("reference");
    }
    reco
}

pub(crate) fn finish_reconstruction(
    sub: &Subject,
    opts: &RunOptions,
    reco: &mut Value,
    kept: Option<&KeptSides>,
    space: EditSpace,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    if let Some(k) = kept {
        crate::manual_correction::merge_kept_sides(reco, k);
    }
    crate::manual_correction::record_reconstruct_inputs(sub, reco, &mut |line| log(tx, line));
    let outcome = write_reconstruction(sub, opts, reco, kept, cancel, tx)?;
    if let Ok(text) = std::fs::read_to_string(sub.reconstruction_json()) {
        if let Ok(written) = serde_json::from_str::<Value>(&text) {
            crate::manual_correction::settle_reconstruct_lock(
                sub,
                &written,
                space,
                opts,
                &mut |line| log(tx, line),
            );
        }
    }
    Ok(outcome)
}

/// How native points are mapped to template space: `ants` requested keeps
/// `antsApplyTransforms`; otherwise `fastwarp-reg` when the normalize record says the transforms came
/// from the wgpu backend (an unreadable file is then an error), and whenever the composite is
/// readable by `fastwarp-reg` (falling back to `antsApplyTransforms` when it is not).
pub(crate) fn normalization_matches_template(sub: &Subject, template: Option<&Path>) -> bool {
    match (sub.norm.template_space.as_deref(), template) {
        (Some(have), Some(t)) => have == crate::template_catalog::infer_space(t),
        _ => true,
    }
}

pub(crate) fn normalized_inverse_composite(
    sub: &Subject,
    template: Option<&Path>,
) -> Option<PathBuf> {
    if !normalization_matches_template(sub, template) {
        return None;
    }
    sub.norm.transform_inverse.clone()
}

pub(crate) fn subject_point_warper<'a>(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&'a AtomicBool>,
    log: &mut dyn FnMut(String),
) -> Result<Box<dyn crate::point_warp::PointWarp + 'a>, CtReconError> {
    let template = opts.normalization_template.clone();
    let req = CtReconRequest {
        warp_route: warp_route_for(sub, opts),
        norm_inverse_composite: normalized_inverse_composite(sub, template.as_deref()),
        template,
        ants_threads: opts.ants_max_threads,
        ..CtReconRequest::new(sub.subj_id.clone(), CtMethod::Auto)
    };
    let req = crate::ct_recon::sanitize_request(&req)?;
    crate::ct_recon::point_warper(&req, cancel, log)
}

pub(crate) fn warp_route_for(sub: &Subject, opts: &RunOptions) -> WarpRoute {
    if opts.backend_for(crate::stages::Stage::Normalize) == RegBackend::Ants {
        return WarpRoute::Ants;
    }
    let made_with_wgpu = read_norm_channels(sub)
        .and_then(|r| {
            r.get("reg_backend")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|b| b == RegBackend::Wgpu.as_str());
    if made_with_wgpu {
        WarpRoute::Rust
    } else {
        WarpRoute::Auto
    }
}

pub(crate) fn trac_windows_error(space: &str, why: impl std::fmt::Display) -> String {
    format!(
        "reconstruct: template space {space} needs the {} bridge for the TRAC search windows: {why}",
        crate::template_catalog::REFERENCE_SPACE
    )
}

fn trac_world_to_reference(
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Option<Mat4>> {
    let Some(template) = opts.normalization_template.as_ref().filter(|t| t.is_file()) else {
        return Ok(None);
    };
    let space = crate::template_catalog::infer_space(template);
    if space == crate::template_catalog::REFERENCE_SPACE {
        return Ok(None);
    }
    let fail = |why: String| PipelineError::Message(trac_windows_error(&space, why));
    match crate::space_bridge::ensure_bridge(template, opts, cancel, tx) {
        Ok(None) => Ok(None),
        Ok(Some(bridge)) => {
            let m = bridge
                .affine_to_reference()
                .map_err(|e| fail(e.to_string()))?;
            log(
                tx,
                format!(
                    "[reconstruct] TRAC windows mapped from {} through the {space} bridge",
                    crate::template_catalog::REFERENCE_SPACE
                ),
            );
            Ok(Some(m))
        }
        Err(PipelineError::Cancelled) => Err(PipelineError::Cancelled),
        Err(e) => Err(fail(e.to_string())),
    }
}

fn recon_err(e: PipelineError) -> PipelineError {
    match e {
        PipelineError::Cancelled => PipelineError::Cancelled,
        PipelineError::Prereq(m) | PipelineError::Message(m) => {
            PipelineError::Message(format!("recon:{m}"))
        }
        PipelineError::StageFailed { message, .. } => {
            PipelineError::Message(format!("recon:{message}"))
        }
        other => PipelineError::Message(format!("recon:{other}")),
    }
}

pub(crate) fn with_space_and_reference(
    reco: &Value,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Value> {
    let Some(template) = opts.normalization_template.as_ref().filter(|t| t.is_file()) else {
        return Ok(reco.clone());
    };
    let mut out = reco.clone();
    let space = crate::template_catalog::infer_space(template);
    let Some(obj) = out.as_object_mut() else {
        return Ok(out);
    };
    if let Some(meta) = obj.get_mut("meta").and_then(Value::as_object_mut) {
        meta.entry("template_space").or_insert_with(|| json!(space));
    }
    if space == crate::template_catalog::REFERENCE_SPACE {
        return Ok(out);
    }
    let Some(mut mni) = obj.get("mni").and_then(Value::as_object).cloned() else {
        return Ok(out);
    };
    for side in mni.values_mut().filter_map(Value::as_object_mut) {
        if let Some(mapped) = side.get("coords_mm_warped").cloned() {
            side.insert("coords_mm".into(), mapped);
            side.remove("coords_mm_warped");
        }
    }
    let warning = |tx: Option<&mpsc::Sender<RunEvent>>, why: String| {
        log(
            tx,
            format!(
                "dbsloc: warning: reconstruct: no reference-space coordinates ({why}); Lead-Tutor needs {} coordinates",
                crate::template_catalog::REFERENCE_SPACE
            ),
        );
    };
    let bridge = match crate::space_bridge::ensure_bridge(template, opts, cancel, tx) {
        Ok(Some(b)) => b,
        Ok(None) => {
            warning(tx, "the template space bridge is unavailable".into());
            return Ok(out);
        }
        Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
        Err(e) => {
            warning(tx, e.to_string());
            return Ok(out);
        }
    };
    match crate::space_bridge::reference_block(&mni, &bridge) {
        Ok(block) => {
            let mut rebuilt = Map::new();
            for (k, v) in std::mem::take(obj) {
                let after_mni = k == "mni";
                rebuilt.insert(k, v);
                if after_mni {
                    rebuilt.insert("reference".into(), Value::Object(block.clone()));
                }
            }
            *obj = rebuilt;
            crate::template_contacts::recompute_template_contacts(
                &mut out,
                "reference",
                opts.el_model.as_deref(),
                &|_| crate::template_contacts::WarpedContacts::Capture,
                &mut |line| log(tx, line),
            );
        }
        Err(e) => warning(tx, e.to_string()),
    }
    Ok(out)
}

fn read_json_file(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn effective_recon_method(reco: &Value, opts: &RunOptions) -> String {
    match reco.get("method").and_then(Value::as_str).unwrap_or("") {
        "trac_core_rust_native" => "trac_core_native".to_owned(),
        "trac_core_rust" => "trac_core".to_owned(),
        "pacer_rust" => "pacer".to_owned(),
        m if m.starts_with("pacer") => "pacer_import".to_owned(),
        _ => opts.recon_method.trim().to_ascii_lowercase(),
    }
}

/// `meta.settings`: the effective choices behind a reconstruction, read from the options and the
/// method logs of the stages it depends on (`null` where a log is missing).
fn reconstruction_settings(sub: &Subject, opts: &RunOptions, reco: &Value) -> Value {
    let norm = read_norm_channels(sub);
    let preprocess_backend = read_json_file(&derivatives::preproc_record(sub)).map(|r| {
        r.get("reg_backend")
            .and_then(Value::as_str)
            .unwrap_or("ants")
            .to_owned()
    });
    let coreg_backend = read_json_file(&derivatives::coreg_method_log(sub))
        .and_then(|r| r.get("engine").and_then(Value::as_str).map(str::to_owned));
    let norm_backend = norm.as_ref().map(|r| {
        r.get("reg_backend")
            .or_else(|| r.get("engine"))
            .and_then(Value::as_str)
            .unwrap_or("ants")
            .to_owned()
    });
    let template = opts.normalization_template.as_deref();
    let template_space = reco["meta"]
        .get("template_space")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| template.map(crate::template_catalog::infer_space));
    let multispectral = norm
        .as_ref()
        .and_then(|r| r.get("channels"))
        .and_then(Value::as_array)
        .map(|c| c.len() > 1);
    json!({
        "recon_method": effective_recon_method(reco, opts),
        "recon_method_requested": opts.recon_method,
        "el_model": opts.el_model,
        "recon_bright_metal": opts.recon_bright_metal
            && sub.postop_modality.as_deref() != Some("CT"),
        "template": template.map(|t| t.to_string_lossy().into_owned()),
        "template_space": template_space,
        "reg_backend": {
            "preprocess": preprocess_backend,
            "coregister": coreg_backend,
            "normalize": norm_backend,
        },
        "norm_isotropic_mm": norm.as_ref().and_then(|r| r.get("norm_isotropic_mm")).cloned(),
        "norm_multispectral": multispectral,
    })
}

/// `meta.software` (the build and time that wrote the file), `meta.run_options` (the options of
/// the reconstruct stage) and `meta.settings` ([`reconstruction_settings`]).
fn stamp_reconstruction_provenance(sub: &Subject, opts: &RunOptions, reco: &mut Value) {
    let settings = reconstruction_settings(sub, opts, reco);
    let mut run_options = serde_json::to_value(opts).unwrap_or(Value::Null);
    if let Some(o) = run_options.as_object_mut() {
        o.remove("el_model_error");
    }
    crate::ct_recon::stamp_software(reco);
    let Some(meta) = reco.get_mut("meta").and_then(Value::as_object_mut) else {
        return;
    };
    meta.insert("run_options".into(), run_options);
    meta.insert("settings".into(), settings);
}

fn write_reconstruction(
    sub: &Subject,
    opts: &RunOptions,
    reco: &Value,
    kept: Option<&KeptSides>,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    std::fs::create_dir_all(&sub.recon_dir)?;
    let out_path = sub.reconstruction_json();
    let mut reco = with_space_and_reference(reco, opts, cancel, tx)?;
    if let Some(k) = kept {
        crate::manual_correction::restore_kept_reference(&mut reco, k);
    }
    stamp_reconstruction_provenance(sub, opts, &mut reco);
    dbsloc_reconstruct::tail_fiducial::mark_tail_fiducial(&mut reco);
    let reco = &reco;
    write_replacing(
        &out_path,
        (serde_json::to_string_pretty(reco)? + "\n").as_bytes(),
    )?;
    log(
        tx,
        format!("[reconstruct] written: {}", file_name_of(&out_path)),
    );
    crate::reco_mat::export_beside_json(&out_path, reco, &mut |line| log(tx, line));
    if let Some(template) = opts.normalization_template.as_ref().filter(|t| t.is_file()) {
        match write_scene_manifest(sub, template, &out_path, opts, cancel, tx) {
            Ok(Some(mp)) => {
                log(
                    tx,
                    format!("[reconstruct] scene manifest: {}", mp.display()),
                );
                if !opts.no_scene3d && !opts.headless {
                    launch_scene3d(&mp, "reconstruct", cancel, tx);
                }
            }
            Ok(None) => {}
            Err(e) => log(
                tx,
                format!("[reconstruct] scene manifest write failed: {e}"),
            ),
        }
    }
    Ok(StageOutcome::done(format!(
        "ok:reconstruct:{}",
        file_name_of(&out_path)
    )))
}

fn reconstruct_template(
    sub: &Subject,
    opts: &RunOptions,
    world_to_reference: Option<Mat4>,
    tx: Option<&mpsc::Sender<RunEvent>>,
    skip: &[&str],
) -> PipelineResult<Value> {
    let fail = |message: String| PipelineError::StageFailed {
        stage: "reconstruct".into(),
        message,
    };
    let electrode = resolve_electrode(opts)?;
    let paths = if normalization_matches_template(sub, opts.normalization_template.as_deref()) {
        match sub.norm_postop_mr_in_use() {
            Some(mr) => vec![mr],
            None => sub.norm_postop_images(),
        }
    } else {
        Vec::new()
    };
    if paths.is_empty() {
        return Err(PipelineError::Prereq(
            "E_RECON_input: no normalized post-op NIfTIs; run normalization or pass paths".into(),
        ));
    }
    log(
        tx,
        format!("[reconstruct] loading {} norm NIfTI(s)", paths.len()),
    );
    let (volumes, inverts, aff) = load_norm_volumes(&paths).map_err(fail)?;
    let lpost = composite_mean_3d(&volumes, &inverts).map_err(|e| fail(e.to_string()))?;
    drop(volumes);

    let metal_dark = !opts.recon_bright_metal;
    log(
        tx,
        format!(
            "[reconstruct] electrode {}: {} contacts in {} level(s), pitch {:.2} mm, head-to-tail span {:.2} mm",
            electrode.elmodel,
            electrode.n_contacts,
            electrode.n_levels(),
            electrode.eldist_mm,
            electrode.fiducial_span_mm()
        ),
    );

    log(tx, "[reconstruct] tracing hemispheres (TRAC/CORE)");
    let trac_cfg = TracConfig {
        world_to_reference,
        ..TracConfig::default()
    };
    let mut sides: Vec<(&'static str, &'static str, SideGeometry)> = Vec::with_capacity(2);
    let mut failed: Vec<(&str, String)> = Vec::new();
    for (hemi, label, key) in [(1u8, "left", SIDE_LEFT), (2u8, "right", SIDE_RIGHT)] {
        if skip.contains(&key) {
            continue;
        }
        let f = match trace_fiducials_flat(&lpost, &aff, hemi, metal_dark, electrode, &trac_cfg) {
            Ok(f) => f,
            Err(e) if !skip.is_empty() => {
                let why = format!("{label}: E_RECON_trace: hemisphere {hemi}: {e}");
                log(
                    tx,
                    format!(
                        "dbsloc: warning: reconstruct: the {label} lead (side {key}) was not reconstructed and is left out of the file: {why}"
                    ),
                );
                failed.push((key, why));
                continue;
            }
            Err(e) => return Err(fail(format!("E_RECON_trace: hemisphere {hemi}: {e}"))),
        };
        let fids = [
            [f[0], f[1], f[2]],
            [f[3], f[4], f[5]],
            [f[6], f[7], f[8]],
            [f[9], f[10], f[11]],
        ];
        let geometry = resolve_fiducials(fids, electrode).map_err(|e| fail(e.to_string()))?;
        sides.push((label, key, geometry));
    }

    let limits = PlausibilityLimits::for_electrode(electrode);
    let (ni, nj, nk) = lpost.dim();
    let bounds = WorldBounds::from_grid(&aff, ni, nj, nk, 0.0);
    let mut problems: Vec<String> = Vec::new();
    for (label, _, side) in &sides {
        for msg in check_side(
            side.head,
            side.tail,
            &side.coords_mm,
            Some(&electrode.contact_level),
            &limits,
            Some(&bounds),
        ) {
            problems.push(format!("{label}: {msg}"));
        }
        if let Some(msg) = check_tilt(side.head, side.tail, MAX_TILT_FROM_VERTICAL_DEG) {
            problems.push(format!("{label}: {msg}"));
        }
    }
    if !problems.is_empty() {
        return Err(fail(implausible_message(&problems)));
    }

    let side_json = |g: &SideGeometry| {
        json!({
            "coords_mm": g.coords_mm,
            "trajectory": g.trajectory,
            "markers": {"head": g.head, "tail": g.tail, "x": g.x, "y": g.y},
            "trajvector": g.trajvector,
        })
    };
    let traced: Vec<&str> = SIDE_KEYS
        .iter()
        .copied()
        .filter(|k| sides.iter().any(|(_, key, _)| key == k))
        .collect();
    let mut props = Map::new();
    for k in &traced {
        props.insert(
            (*k).to_owned(),
            json!({"elmodel": electrode.elmodel, "manually_corrected": false}),
        );
    }
    let mut meta = Map::new();
    meta.insert(
        "norm_inputs".into(),
        json!(paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()),
    );
    meta.insert(
        "nifti_aff_4x4".into(),
        json!(aff.iter().map(|r| r.to_vec()).collect::<Vec<_>>()),
    );
    meta.insert("lead_diameter_mm".into(), json!(electrode.lead_diameter_mm));
    meta.insert("eldist_mm".into(), json!(electrode.eldist_mm));
    meta.insert("elmodel".into(), json!(electrode.elmodel));
    meta.insert("n_contacts".into(), json!(electrode.n_contacts));
    meta.insert("level_z_mm".into(), json!(electrode.level_z_mm));
    meta.insert("contact_level".into(), json!(electrode.contact_level));
    meta.insert("is_directed".into(), json!(electrode.is_directed));
    meta.insert("tracer".into(), json!(TRACER_TRAC_CORE));
    meta.insert("side_keys".into(), json!(SIDE_KEYS_CONVENTION));
    if electrode.is_directed {
        meta.insert("orientation".into(), json!(ORIENTATION_UNRESOLVED));
    }
    let mut status: Vec<(&str, &str)> = traced
        .iter()
        .map(|k| (*k, crate::ct_recon::SIDE_TRACED))
        .collect();
    status.extend(
        failed
            .iter()
            .map(|(k, _)| (*k, crate::ct_recon::SIDE_FAILED)),
    );
    let flags: Vec<(&str, Vec<&str>)> = traced.iter().map(|k| (*k, Vec::new())).collect();
    meta.insert(
        "reconstruct".into(),
        crate::ct_recon::reconstruct_meta(&status, &failed, &flags),
    );
    let mut mni = Map::new();
    for key in [SIDE_RIGHT, SIDE_LEFT] {
        if let Some((_, _, g)) = sides.iter().find(|(_, k, _)| *k == key) {
            mni.insert(key.into(), side_json(g));
        }
    }
    let mut reco = json!({
        "version": 2,
        "subj_id": sub.subj_id,
        "method": TRACER_TRAC_CORE,
        "mni": Value::Object(mni),
        "props": Value::Object(props),
        "meta": Value::Object(meta),
    });
    crate::template_contacts::recompute_template_contacts(
        &mut reco,
        "mni",
        None,
        &|_| crate::template_contacts::WarpedContacts::Omit,
        &mut |line| log(tx, line),
    );
    Ok(reco)
}

pub(crate) fn affine_4x4(hdr: &nifti::NiftiHeader) -> Mat4 {
    fastwarp_core::nifti_affine::header_affine(hdr)
}

fn ct_invert_from_filename(path: &Path) -> bool {
    file_name_of(path).to_uppercase().contains("CT")
}

#[allow(clippy::type_complexity)]
fn load_norm_volumes(paths: &[PathBuf]) -> Result<(Vec<Array3<f64>>, Vec<bool>, Mat4), String> {
    let mut vols: Vec<Array3<f64>> = Vec::new();
    let mut inverts = Vec::new();
    let mut first: Option<(Mat4, [usize; 3])> = None;
    for p in paths {
        let (data, aff) = load_volume_f64(p).map_err(|e| e.to_string())?;
        let shape = [data.shape()[0], data.shape()[1], data.shape()[2]];
        match first {
            None => first = Some((aff, shape)),
            Some((a0, s0)) => {
                let close = a0
                    .iter()
                    .flatten()
                    .zip(aff.iter().flatten())
                    .all(|(x, y)| (x - y).abs() <= 1e-4 + 1e-5 * y.abs());
                if s0 != shape || !close {
                    return Err(format!(
                        "NIfTI grid mismatch: {} vs first volume (shape/affine differ)",
                        file_name_of(p)
                    ));
                }
            }
        }
        inverts.push(ct_invert_from_filename(p));
        vols.push(data);
    }
    let (aff, _) =
        first.ok_or_else(|| "composite_lpost: need at least one NIfTI path".to_owned())?;
    Ok((vols, inverts, aff))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(|v| absolute(Path::new(&v)))
}

fn write_scene_manifest(
    sub: &Subject,
    template: &Path,
    reco_json: &Path,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Option<PathBuf>> {
    let mut bridge = || {
        crate::space_bridge::ensure_bridge(template, opts, cancel, tx).map_err(|e| match e {
            PipelineError::Cancelled => None,
            other => Some(other.to_string()),
        })
    };
    let mut logs = Vec::new();
    let sets = crate::atlas_catalog::bundled_and_external_atlas_sets(
        opts.leaddbs_atlases_dir.as_deref(),
        &mut |m| logs.push(m),
    );
    for m in logs {
        log(tx, format!("dbsloc: warning: scene: {m}"));
    }
    write_scene_manifest_with(sub, template, reco_json, sets, &mut bridge, tx)
}

type BridgeLookup<'a> =
    dyn FnMut() -> Result<Option<crate::space_bridge::Bridge>, Option<String>> + 'a;

fn is_nifti_name(p: &Path) -> bool {
    let n = file_name_of(p).to_ascii_lowercase();
    n.ends_with(".nii") || n.ends_with(".nii.gz")
}

const ATLAS_LAYOUT_SUBDIRS: [&str; 4] = ["lh", "rh", "mixed", "midline"];

fn atlas_mask_files(root: &Path) -> Vec<PathBuf> {
    let mut rel: Vec<PathBuf> = Vec::new();
    let mut scan = |dir: &Path, prefix: &Path| {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() && is_nifti_name(&p) {
                rel.push(prefix.join(e.file_name()));
            }
        }
    };
    scan(root, Path::new(""));
    for sub in ATLAS_LAYOUT_SUBDIRS {
        scan(&root.join(sub), Path::new(sub));
    }
    rel.sort();
    rel
}

fn atlas_cache_folder(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    out.truncate(64);
    format!("{out}-{}", &crate::sha256::sha256_hex(id.as_bytes())[..12])
}

fn mirrored_atlas_dir(bridge: &crate::space_bridge::Bridge, id: &str) -> PathBuf {
    bridge
        .dir
        .join("atlases")
        .join(atlas_cache_folder(id))
        .join("structures")
}

pub(crate) fn warp_atlas_into_template(
    bridge: &crate::space_bridge::Bridge,
    atlas_id: &str,
    atlas_dir: &Path,
    template: &Path,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<PathBuf, String> {
    let files = atlas_mask_files(atlas_dir);
    if files.is_empty() {
        return Err(format!("no mask NIfTI in {}", atlas_dir.display()));
    }
    let dest = mirrored_atlas_dir(bridge, atlas_id);
    if files.iter().all(|f| dest.join(f).is_file()) {
        return Ok(dest);
    }
    let parent = dest
        .parent()
        .ok_or_else(|| format!("no parent for {}", dest.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let staging = parent.join(".partial-structures");
    let _ = std::fs::remove_dir_all(&staging);
    log(
        tx,
        format!(
            "dbsloc: note: scene: warping {} atlas masks of {atlas_id} into {}",
            files.len(),
            bridge.target_space
        ),
    );
    let mut jobs = Vec::with_capacity(files.len());
    for f in &files {
        let out = staging.join(f);
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        jobs.push((atlas_dir.join(f), out));
    }
    if let Err(e) = bridge.warp_images_to_target(&jobs, template, false) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(dest)
}

fn scene_vta_entries(sub: &Subject, space: &str) -> Vec<Value> {
    let root = crate::vat::settings::stimulations_dir(&sub.subj_dir).join(space);
    let Ok(labels) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut newest: Option<(std::time::SystemTime, Vec<Value>)> = None;
    for label in labels.filter_map(Result::ok) {
        let name = label.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !label.path().is_dir() {
            continue;
        }
        let mut entries = Vec::new();
        let mut stamp: Option<std::time::SystemTime> = None;
        for (side, letter) in [("right", 'R'), ("left", 'L')] {
            let suffix = format!("_hemi-{letter}.nii");
            let Ok(files) = std::fs::read_dir(label.path()) else {
                continue;
            };
            let found = files.filter_map(Result::ok).find(|f| {
                let n = f.file_name().to_string_lossy().into_owned();
                n.contains("_sim-binary_model-") && n.ends_with(&suffix)
            });
            if let Some(f) = found {
                if let Ok(t) = f.metadata().and_then(|m| m.modified()) {
                    stamp = Some(stamp.map_or(t, |s| s.max(t)));
                }
                entries.push(json!({
                    "side": side,
                    "label": name,
                    "path": absolute(&f.path()).to_string_lossy(),
                }));
            }
        }
        if let Some(t) = stamp {
            if newest.as_ref().is_none_or(|(best, _)| t > *best) {
                newest = Some((t, entries));
            }
        }
    }
    newest.map(|(_, e)| e).unwrap_or_default()
}

fn write_scene_manifest_with(
    sub: &Subject,
    template: &Path,
    reco_json: &Path,
    atlas_sets: Vec<crate::atlas_catalog::AtlasSetEntry>,
    bridge: &mut BridgeLookup<'_>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<Option<PathBuf>> {
    use crate::atlas_catalog::{AtlasLayout, AtlasSetEntry};

    if !template.is_file() || !reco_json.is_file() {
        return Ok(None);
    }
    let manifest_path = derivatives::scene_manifest(sub);
    if let Some(dir) = manifest_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let space = crate::template_catalog::infer_space(template);

    let mut man = Map::new();
    man.insert("manifest_version".into(), json!(1));
    man.insert("subj_id".into(), json!(sub.subj_id));
    man.insert("space".into(), json!(space));
    man.insert(
        "template_nii".into(),
        json!(absolute(template).to_string_lossy()),
    );
    man.insert(
        "reco_json".into(),
        json!(absolute(reco_json).to_string_lossy()),
    );
    let backdrops: Vec<_> = crate::backdrops::scene_backdrops(sub, template, &space)
        .into_iter()
        .map(|mut b| {
            b.path = absolute(&b.path);
            b
        })
        .collect();
    man.insert("backdrops".into(), json!(backdrops));
    let vta = scene_vta_entries(sub, &space);
    if !vta.is_empty() {
        man.insert("vta".into(), Value::Array(vta));
    }
    if let Some(d) = env_path("DBSLOC_SCENE_ATLAS_OBJ_DIR") {
        man.insert("atlas_obj_dir".into(), json!(d.to_string_lossy()));
    }
    if let Some(n) = env_path("DBSLOC_SCENE_ATLAS_NII") {
        man.insert("atlas_nii".into(), json!(n.to_string_lossy()));
    }
    let mut sets: Vec<AtlasSetEntry> = Vec::new();
    if let Some(p) = env_path("DBSLOC_SCENE_ATLAS_LEAD_DIR").filter(|p| p.is_dir()) {
        let index_path = p.join(crate::atlas_index::ATLAS_INDEX_FILE);
        let index = index_path
            .is_file()
            .then(|| crate::atlas_index::AtlasIndex::read(&index_path).ok())
            .flatten();
        sets.push(AtlasSetEntry {
            id: "override".to_owned(),
            name: file_name_of(&p),
            dir: absolute(&p),
            layout: AtlasLayout::Lead,
            index,
            default_on_substrings: Some(
                crate::atlas_catalog::AHEAD_DEFAULT_ON
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            citation: None,
        });
    }
    sets.extend(atlas_sets.into_iter().filter(|s| s.dir.is_dir()));
    let in_reference = space == crate::template_catalog::REFERENCE_SPACE;
    let mut placed: Vec<AtlasSetEntry> = Vec::with_capacity(sets.len());
    for mut set in sets {
        if set.id == "override" || in_reference {
            set.dir = absolute(&set.dir);
            placed.push(set);
            continue;
        }
        let warn = |why: &str| {
            log(
                tx,
                format!(
                    "dbsloc: warning: scene: no atlas {} for template space {space}: {why}",
                    set.name
                ),
            );
        };
        match bridge() {
            Ok(Some(b)) => match warp_atlas_into_template(&b, &set.id, &set.dir, template, tx) {
                Ok(dir) => {
                    set.dir = absolute(&dir);
                    placed.push(set);
                }
                Err(e) => warn(&e),
            },
            Ok(None) => warn("the template space bridge is unavailable"),
            Err(None) => return Err(PipelineError::Cancelled),
            Err(Some(e)) => warn(&e),
        }
    }
    if !placed.is_empty() {
        if let Some(def) = crate::atlas_catalog::default_atlas(&placed) {
            man.insert("atlas_default_id".into(), json!(def.id));
        }
        man.insert(
            "atlas_lead_sets".into(),
            json!(crate::atlas_catalog::atlas_lead_sets_json(&placed)),
        );
    }
    write_replacing(
        &manifest_path,
        serde_json::to_string_pretty(&Value::Object(man))?.as_bytes(),
    )?;
    Ok(Some(manifest_path))
}

pub fn run_visualize(
    sub: &Subject,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> PipelineResult<StageOutcome> {
    let Some(template) = opts.normalization_template.as_ref().filter(|t| t.is_file()) else {
        log(tx, "[visualize] no template set — skipping scene manifest");
        return Ok(StageOutcome::skipped(
            "visualize: no normalization template set",
        ));
    };
    let reco_json = sub.reconstruction_json();
    match write_scene_manifest(sub, template, &reco_json, opts, cancel, tx)? {
        Some(manifest) => {
            log(
                tx,
                format!("[visualize] scene manifest: {}", manifest.display()),
            );
            Ok(StageOutcome::done(format!(
                "ok:visualize:{}",
                file_name_of(&manifest)
            )))
        }
        None => Ok(StageOutcome::skipped(
            "visualize: scene manifest not written (template or reco missing)",
        )),
    }
}

fn launch_scene3d(
    manifest: &Path,
    stage: &str,
    cancel: Option<&AtomicBool>,
    tx: Option<&mpsc::Sender<RunEvent>>,
) {
    let note = |n: String| log(tx, format!("dbsloc: note: {stage}: scene3d: {n}"));
    let (display_ok, probe_note) = has_display();
    if let Some(n) = probe_note {
        log(tx, format!("dbsloc: note: {n}"));
    }
    if !display_ok {
        let skip = std::env::var("DBSLOC_SCENE3D_SKIP_NO_DISPLAY")
            .map(|v| {
                matches!(
                    v.trim().to_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if !skip {
            note(format!(
                "no display available; viewer not launched (manifest written: {}). Open it \
                 later with dbsloc viz scene --manifest <path>",
                manifest.display()
            ));
        }
        return;
    }
    let Some(exe) = gui_binary() else {
        note(
            "dbs-localizer not found; viewer not launched (cargo build -p dbsloc-app --release \
             or set DBSLOC_GUI_BIN)"
                .to_owned(),
        );
        return;
    };
    log(tx, format!("[{stage}] launching {}", exe.display()));
    let mut cmd = Command::new(exe);
    cmd.arg("scene").arg("--manifest").arg(absolute(manifest));
    match crate::display::run_viewer(&mut cmd, cancel) {
        crate::display::ViewerOutcome::Exited(0) => {}
        crate::display::ViewerOutcome::Exited(code) => note(format!("viewer exited {code}")),
        crate::display::ViewerOutcome::Cancelled => note("viewer closed: run cancelled".to_owned()),
        crate::display::ViewerOutcome::Failed(e) => note(format!("viewer failed to start: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, Subject) {
        let d = std::env::temp_dir().join(format!("dbsloc_stage_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let anat = d.join("rawdata/sub-1/ses-preop/anat");
        std::fs::create_dir_all(&anat).unwrap();
        std::fs::write(anat.join("sub-1_ses-preop_T1w.nii.gz"), b"x").unwrap();
        let sub = crate::bids::resolve_subject_with(&d, "1", false).unwrap();
        (d, sub)
    }

    fn install_prefs(sub: &Subject) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fastwarp/crates/fastwarp-matfile/tests/data/uiprefs_contiguous.mat");
        std::fs::create_dir_all(&sub.prefs_dir).unwrap();
        std::fs::copy(src, sub.uiprefs_path()).unwrap();
    }

    fn opts_without_model() -> RunOptions {
        RunOptions::new("/data", "1")
    }

    #[test]
    fn electrode_model_comes_from_uiprefs_when_unset() {
        let (root, sub) = scratch("prefs_model");
        install_prefs(&sub);
        let (tx, rx) = mpsc::channel();
        let resolved = opts_with_prefs_model(&sub, &opts_without_model(), Some(&tx)).unwrap();
        assert_eq!(
            resolved.el_model.as_deref(),
            Some("Boston Scientific Vercise Directed")
        );
        drop(tx);
        let lines: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                RunEvent::LogLine { line } => Some(line),
                _ => None,
            })
            .collect();
        assert!(
            lines.iter().any(
                |l| l.starts_with("dbsloc: note: reconstruct: electrode model from ")
                    && l.ends_with(": Boston Scientific Vercise Directed")
            ),
            "{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_electrode_model_wins_over_uiprefs() {
        let (root, sub) = scratch("prefs_explicit");
        install_prefs(&sub);
        let mut o = opts_without_model();
        o.el_model = Some("Medtronic 3387".into());
        assert!(opts_with_prefs_model(&sub, &o, None).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_uiprefs_keeps_the_existing_error() {
        let (root, sub) = scratch("prefs_missing");
        assert!(opts_with_prefs_model(&sub, &opts_without_model(), None).is_none());
        let err = resolve_electrode(&opts_without_model())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no electrode model set"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_manifest_needs_template_and_reco() {
        let (root, sub) = scratch("manifest_none");
        let tpl = root.join("t1.nii.gz");
        let reco = sub.reconstruction_json();
        let mut no_bridge = || Ok(None);
        assert!(
            write_scene_manifest_with(&sub, &tpl, &reco, Vec::new(), &mut no_bridge, None)
                .unwrap()
                .is_none()
        );
        std::fs::write(&tpl, b"x").unwrap();
        assert!(
            write_scene_manifest_with(&sub, &tpl, &reco, Vec::new(), &mut no_bridge, None)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_manifest_records_absolute_inputs_and_atlas_sets() {
        let (root, sub) = scratch("manifest");
        let tpl = root.join("t1.nii.gz");
        std::fs::write(&tpl, b"x").unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let reco = sub.reconstruction_json();
        std::fs::write(&reco, b"{}").unwrap();
        let mut no_bridge = || Ok(None);
        let p = write_scene_manifest_with(&sub, &tpl, &reco, Vec::new(), &mut no_bridge, None)
            .unwrap()
            .unwrap();
        assert!(p.ends_with(Path::new("reconstruction/viz/scene_manifest.json")));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["manifest_version"], json!(1));
        assert_eq!(v["subj_id"], json!("1"));
        assert_eq!(v["reco_json"], json!(absolute(&reco).to_string_lossy()));
        assert_eq!(v["template_nii"], json!(absolute(&tpl).to_string_lossy()));
        assert_eq!(
            v["space"],
            json!(crate::template_catalog::infer_space(&tpl))
        );
        if let Some(sets) = v.get("atlas_lead_sets") {
            assert_eq!(sets[0]["default_on_substrings"][0], json!("STN"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_manifest_lists_the_atlases_directory_of_the_run_options() {
        let (root, sub) = scratch("manifest_external_atlases");
        let tpl = root.join("tpl").join("t1.nii.gz");
        std::fs::create_dir_all(tpl.parent().unwrap()).unwrap();
        std::fs::write(&tpl, b"x").unwrap();
        std::fs::write(
            tpl.with_file_name("template.json"),
            format!(
                r#"{{"space": "{}"}}"#,
                crate::template_catalog::REFERENCE_SPACE
            ),
        )
        .unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let reco = sub.reconstruction_json();
        std::fs::write(&reco, b"{}").unwrap();
        let atlases = root.join("my_atlases");
        std::fs::create_dir_all(atlases.join("Test Atlas (Doe 2020)").join("lh")).unwrap();
        let ids = |opts: &RunOptions| -> Vec<String> {
            let p = write_scene_manifest(&sub, &tpl, &reco, opts, None, None)
                .unwrap()
                .unwrap();
            manifest_json(&p)["atlas_lead_sets"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s["id"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut opts = RunOptions::new(&root, "1");
        opts.leaddbs_atlases_dir = Some(atlases.clone());
        assert!(
            ids(&opts).contains(&"external:Test Atlas (Doe 2020)".to_owned()),
            "{:?}",
            ids(&opts)
        );
        opts.leaddbs_atlases_dir = None;
        assert!(!ids(&opts).iter().any(|id| id.starts_with("external:")));
        let _ = std::fs::remove_dir_all(&root);
    }

    const ID3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn atlas_grid() -> fastwarp_reg::Grid {
        fastwarp_reg::Grid::new([40, 40, 40], [1.0; 3], [20.0, 20.0, -20.0], ID3).unwrap()
    }

    fn write_blob_mask(path: &Path) {
        let g = atlas_grid();
        let data: Vec<f32> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let d2 = (i as f64 - 20.0).powi(2)
                    + (j as f64 - 18.0).powi(2)
                    + (k as f64 - 22.0).powi(2);
                (-d2 / (2.0 * 3.0f64.powi(2))).exp() as f32
            })
            .collect();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        fastwarp_reg::Volume::from_vec(g, data)
            .unwrap()
            .save_nifti(path)
            .unwrap();
    }

    fn centroid_ras(vol: &fastwarp_reg::Volume<f32>) -> [f64; 3] {
        let g = vol.grid();
        let (mut sum, mut acc) = (0.0f64, [0.0f64; 3]);
        for (lin, v) in vol.data().iter().enumerate() {
            let w = f64::from(*v);
            let [i, j, k] = g.ijk(lin);
            let p = fastwarp_reg::lps_to_ras(g.index_to_point([i as f64, j as f64, k as f64]));
            sum += w;
            for a in 0..3 {
                acc[a] += w * p[a];
            }
        }
        [acc[0] / sum, acc[1] / sum, acc[2] / sum]
    }

    fn translation_bridge(dir: &Path, t_lps: [f64; 3]) -> crate::space_bridge::Bridge {
        use fastwarp_reg::transform::{
            write_composite_pair, AffineTransform, CompositePair, TransformChain, TransformElem,
        };
        let affine = AffineTransform::from_translation(t_lps);
        let inverse = affine.inverse().unwrap();
        let pair = CompositePair {
            forward: TransformChain::from_queue(vec![TransformElem::Affine(affine)]),
            inverse: TransformChain::from_queue(vec![TransformElem::Affine(inverse)]),
        };
        std::fs::create_dir_all(dir).unwrap();
        let prefix = crate::space_bridge::bridge_prefix(dir);
        write_composite_pair(&prefix, &pair).unwrap();
        crate::space_bridge::Bridge {
            dir: dir.to_path_buf(),
            target_space: "TestSpace".into(),
            prefix,
        }
    }

    fn manifest_json(p: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    fn flat_set(dir: &Path) -> Vec<crate::atlas_catalog::AtlasSetEntry> {
        vec![crate::atlas_catalog::AtlasSetEntry {
            id: crate::atlas_catalog::AHEAD_ATLAS_ID.to_owned(),
            name: crate::atlas_catalog::AHEAD_ATLAS_NAME.to_owned(),
            dir: dir.to_path_buf(),
            layout: crate::atlas_catalog::AtlasLayout::Flat,
            index: None,
            default_on_substrings: None,
            citation: None,
        }]
    }

    #[test]
    fn scene_manifest_keeps_the_reference_atlas_in_the_reference_space() {
        let (root, sub) = scratch("manifest_ref_atlas");
        let tpl = root.join("space/MNI152NLin2009bAsym/t1.nii.gz");
        std::fs::create_dir_all(tpl.parent().unwrap()).unwrap();
        std::fs::write(&tpl, b"x").unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let reco = sub.reconstruction_json();
        std::fs::write(&reco, b"{}").unwrap();
        let atlas = root.join("atlas/structures");
        write_blob_mask(&atlas.join("mask.nii.gz"));
        let mut never = || -> Result<Option<crate::space_bridge::Bridge>, Option<String>> {
            panic!("the reference space needs no bridge")
        };
        let p = write_scene_manifest_with(&sub, &tpl, &reco, flat_set(&atlas), &mut never, None)
            .unwrap()
            .unwrap();
        let v = manifest_json(&p);
        assert_eq!(v["space"], json!("MNI152NLin2009bAsym"));
        assert_eq!(
            v["atlas_lead_sets"][0]["dir"],
            json!(absolute(&atlas).to_string_lossy())
        );
        assert_eq!(
            v["atlas_lead_sets"][0]["name"],
            json!(crate::atlas_catalog::AHEAD_ATLAS_NAME)
        );
        assert_eq!(v["atlas_default_id"], json!("ahead"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_manifest_warps_the_atlas_into_a_non_reference_template() {
        let (root, sub) = scratch("manifest_warp_atlas");
        let tpl = root.join("t1.nii.gz");
        fastwarp_reg::Volume::<f32>::zeros(atlas_grid())
            .save_nifti(&tpl)
            .unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let reco = sub.reconstruction_json();
        std::fs::write(&reco, b"{}").unwrap();
        let atlas = root.join("atlas/structures");
        write_blob_mask(&atlas.join("mask.nii.gz"));
        std::fs::write(atlas.join("readme.txt"), b"not a mask").unwrap();
        let t_lps = [3.0, -2.0, 4.0];
        let bridge = translation_bridge(&root.join("bridge"), t_lps);
        let bridge_dir = bridge.dir.clone();
        let (tx, rx) = mpsc::channel();
        let mut lookup = || Ok(Some(bridge.clone()));
        let p =
            write_scene_manifest_with(&sub, &tpl, &reco, flat_set(&atlas), &mut lookup, Some(&tx))
                .unwrap()
                .unwrap();
        let v = manifest_json(&p);
        assert_eq!(
            v["space"],
            json!(crate::template_catalog::infer_space(&tpl))
        );
        let dir = PathBuf::from(v["atlas_lead_sets"][0]["dir"].as_str().unwrap());
        assert!(dir.starts_with(absolute(&bridge_dir)), "{}", dir.display());
        assert!(!dir.join("readme.txt").exists());
        let warped = fastwarp_reg::Volume::<f32>::load_nifti(&dir.join("mask.nii.gz")).unwrap();
        let source = fastwarp_reg::Volume::<f32>::load_nifti(&atlas.join("mask.nii.gz")).unwrap();
        let (c, q) = (centroid_ras(&source), centroid_ras(&warped));
        let t_ras = [-t_lps[0], -t_lps[1], t_lps[2]];
        for a in 0..3 {
            assert!(
                (q[a] - (c[a] - t_ras[a])).abs() < 0.5,
                "axis {a}: {} vs {}",
                q[a],
                c[a] - t_ras[a]
            );
        }
        assert!(rx.try_iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("warping 1 atlas masks of ahead")
        )));

        let (tx2, rx2) = mpsc::channel();
        let mut again = || Ok(Some(bridge.clone()));
        write_scene_manifest_with(&sub, &tpl, &reco, flat_set(&atlas), &mut again, Some(&tx2))
            .unwrap()
            .unwrap();
        assert!(!rx2.try_iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("warping")
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scene_manifest_drops_the_atlas_when_the_bridge_fails() {
        let (root, sub) = scratch("manifest_no_bridge_atlas");
        let tpl = root.join("t1.nii.gz");
        std::fs::write(&tpl, b"x").unwrap();
        std::fs::create_dir_all(&sub.recon_dir).unwrap();
        let reco = sub.reconstruction_json();
        std::fs::write(&reco, b"{}").unwrap();
        let atlas = root.join("atlas/structures");
        write_blob_mask(&atlas.join("mask.nii.gz"));
        let (tx, rx) = mpsc::channel();
        let mut failing = || Err(Some("no registration".to_owned()));
        let p =
            write_scene_manifest_with(&sub, &tpl, &reco, flat_set(&atlas), &mut failing, Some(&tx))
                .unwrap()
                .unwrap();
        assert!(manifest_json(&p).get("atlas_lead_sets").is_none());
        let space = crate::template_catalog::infer_space(&tpl);
        let want = format!(
            "dbsloc: warning: scene: no atlas {} for template space {space}: no registration",
            crate::atlas_catalog::AHEAD_ATLAS_NAME
        );
        assert!(rx.try_iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line == want
        )));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trac_windows_error_names_the_space_and_the_bridge() {
        assert_eq!(
            trac_windows_error("Foo", "boom"),
            "reconstruct: template space Foo needs the MNI152NLin2009bAsym bridge for the TRAC \
             search windows: boom"
        );
    }

    #[test]
    fn outputs_are_replaced_not_written_through_hardlinks() {
        let (root, _sub) = scratch("hardlink");
        let original = root.join("original.nii.gz");
        let output = root.join("output.nii.gz");
        std::fs::write(&original, b"input data").unwrap();
        std::fs::hard_link(&original, &output).unwrap();
        unlink_outputs(std::slice::from_ref(&output)).unwrap();
        std::fs::write(&output, b"stage output").unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"input data");
        unlink_outputs(&[root.join("never_existed.nii.gz")]).unwrap();
        let lock = root.join("locks").join("coreg.json");
        crate::locks::write_approval(&lock, true, None, &[], Some(1.0)).unwrap();
        let alias = root.join("alias.json");
        std::fs::hard_link(&lock, &alias).unwrap();
        crate::locks::write_approval(&lock, false, None, &[], Some(2.0)).unwrap();
        assert!(std::fs::read_to_string(&alias).unwrap().contains("true"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oom_kill_gets_the_mitigation_hint() {
        let e = with_oom_hint(PipelineError::ToolFailed {
            tool: "antsRegistration".into(),
            code: -9,
            stderr_tail: String::new(),
        });
        assert!(e.to_string().contains("--norm-isotropic-mm"), "{e}");
        let e = with_oom_hint(PipelineError::ToolFailed {
            tool: "antsApplyTransforms".into(),
            code: -9,
            stderr_tail: String::new(),
        });
        assert!(
            e.to_string().starts_with("antsApplyTransforms killed"),
            "{e}"
        );
        let e = with_oom_hint(PipelineError::ToolFailed {
            tool: "antsRegistration".into(),
            code: 1,
            stderr_tail: String::new(),
        });
        assert_eq!(e.to_string(), "antsRegistration failed with 1");
    }

    const IDENT: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    fn nii(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut d = ndarray::Array3::<u8>::zeros((6, 6, 6));
        d[[2, 3, 3]] = 90;
        d[[3, 3, 3]] = 120;
        crate::nifti_io::write_u8_3d(p, &d, &IDENT).unwrap();
    }

    struct NormCase {
        root: PathBuf,
        sub: Subject,
        opts: RunOptions,
        t1_tpl: PathBuf,
        t2_tpl: PathBuf,
    }

    fn norm_case(name: &str, with_t2: bool, with_ct: bool) -> NormCase {
        norm_case_with(name, with_t2, with_ct, nii)
    }

    fn norm_case_with(name: &str, with_t2: bool, with_ct: bool, write: fn(&Path)) -> NormCase {
        let root = std::env::temp_dir().join(format!("dbsloc_norm_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let anat = root.join("rawdata/sub-1/ses-preop/anat");
        write(&anat.join("sub-1_ses-preop_T1w.nii.gz"));
        if with_t2 {
            write(&anat.join("sub-1_ses-preop_T2w.nii.gz"));
        }
        let t1_tpl = root.join("tpl/t1.nii.gz");
        let t2_tpl = root.join("tpl/t2.nii.gz");
        write(&t1_tpl);
        write(&t2_tpl);
        if with_ct {
            write(&root.join("rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_CT.nii.gz"));
        }
        let mut sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        if with_ct {
            write(&derivatives::coreg_ct_image(&sub));
            sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        }
        let opts = RunOptions {
            normalization_template: Some(absolute(&t1_tpl)),
            normalization_template_t2: crate::templates::select_template_t2_with(
                None,
                &std::collections::HashMap::new(),
                Some(&absolute(&t1_tpl)),
            )
            .map(|r| r.path),
            ..RunOptions::new(&root, "1")
        };
        NormCase {
            t1_tpl: absolute(&t1_tpl),
            t2_tpl: absolute(&t2_tpl),
            root,
            sub,
            opts,
        }
    }

    fn arg_after(argv: &[String], flag: &str) -> String {
        let i = argv.iter().position(|a| a == flag).unwrap();
        argv[i + 1].clone()
    }

    fn fake_norm_affine() -> fastwarp_reg::transform::AffineTransform {
        fastwarp_reg::transform::AffineTransform::new(
            [[1.1, 0.02, 0.0], [-0.01, 0.95, 0.03], [0.0, 0.04, 1.05]],
            [2.0, -3.0, 5.5],
            [1.0, -4.0, 2.0],
        )
    }

    fn fake_norm_pair() -> fastwarp_reg::transform::CompositePair {
        let grid = fastwarp_reg::grid::Grid::new(
            [4, 4, 4],
            [20.0; 3],
            [-30.0; 3],
            fastwarp_reg::linalg::IDENTITY3,
        )
        .unwrap();
        fastwarp_reg::transform::CompositePair::new(
            &fake_norm_affine(),
            fastwarp_reg::transform::DisplacementField::zeros(grid),
            fastwarp_reg::transform::DisplacementField::zeros(grid),
        )
        .unwrap()
    }

    fn fake_ants(
        calls: &mut Vec<Vec<String>>,
        image: PathBuf,
        fail_syn: bool,
    ) -> impl FnMut(&[String]) -> PipelineResult<()> + '_ {
        move |argv: &[String]| {
            calls.push(argv.to_vec());
            if argv[0] == "antsApplyTransforms" {
                std::fs::copy(&image, arg_after(argv, "-o")).unwrap();
                return Ok(());
            }
            let syn = argv.iter().any(|a| a == "SyN[0.1,3,0]");
            if syn && fail_syn {
                return Err(PipelineError::ToolFailed {
                    tool: "antsRegistration".into(),
                    code: 1,
                    stderr_tail: String::new(),
                });
            }
            let out = arg_after(argv, "--output");
            let (pfx, warped) = out
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split_once(',')
                .unwrap();
            std::fs::copy(&image, warped).unwrap();
            if syn {
                fastwarp_reg::transform::write_composite_pair(Path::new(pfx), &fake_norm_pair())
                    .unwrap();
            } else {
                std::fs::write(format!("{pfx}0GenericAffine.mat"), b"tx").unwrap();
            }
            Ok(())
        }
    }

    fn tools() -> NormTools<'static> {
        NormTools {
            ants: Some(PathBuf::from("antsRegistration")),
            apply: Some(PathBuf::from("antsApplyTransforms")),
            reg: RegChoice::ants(RegBackend::Ants),
            cancel: None,
        }
    }

    fn run_fake(
        case: &NormCase,
        fail_syn: bool,
    ) -> (PipelineResult<StageOutcome>, Vec<Vec<String>>) {
        let mut calls = Vec::new();
        let image = case.sub.anchor_best().unwrap().to_path_buf();
        let res = {
            let mut exec = fake_ants(&mut calls, image, fail_syn);
            normalize_with(&case.sub, &case.opts, &tools(), &mut exec, None)
        };
        (res, calls)
    }

    fn no_partials(dir: &Path) -> bool {
        std::fs::read_dir(dir).map_or(true, |rd| {
            rd.flatten()
                .all(|e| !crate::outputs::is_partial_name(&e.file_name().to_string_lossy()))
        })
    }

    #[test]
    fn multispectral_normalize_coregisters_t2_then_runs_two_channel_syn() {
        let case = norm_case("ms", true, true);
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        let sub = &case.sub;
        let anchor = sub.anchor_best().unwrap();
        let t2_src = sub.preop_t2_nonanchor().unwrap();
        let t2_pfx = coreg_work_prefix(sub, NORM_T2_CHANNEL);
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let norm_pfx = norm_work_prefix(sub);
        assert_eq!(calls.len(), 3, "{calls:#?}");
        assert_eq!(
            calls[0],
            build_ants_mr_coreg_argv(Path::new("antsRegistration"), t2_src, anchor, &t2_pfx, &[],)
        );
        assert_eq!(
            calls[1],
            build_ants_syn_multispectral_argv(
                Path::new("antsRegistration"),
                anchor,
                &case.t1_tpl,
                &suffixed(&t2_pfx, "_Warped.nii.gz"),
                &case.t2_tpl,
                &norm_pfx,
                &[],
            )
        );
        assert_eq!(calls[2][0], "antsApplyTransforms");
        for f in [
            derivatives::coreg_t2_affine(sub),
            derivatives::coreg_t2_image(sub),
            derivatives::norm_anchor_image(sub, &space),
            derivatives::norm_forward(sub, &space),
            derivatives::norm_inverse(sub, &space),
            derivatives::norm_postop_image(sub, "CT", &space),
            derivatives::norm_method_log(sub),
        ] {
            assert!(f.is_file(), "{}", f.display());
        }
        assert!(
            no_partials(&sub.coreg_dir)
                && no_partials(&sub.norm_dir)
                && no_partials(&derivatives::coreg_transform_dir(sub))
                && no_partials(&derivatives::norm_transform_dir(sub))
        );
        let rec = read_norm_channels(sub).unwrap();
        assert_eq!(rec["approval"], json!(0));
        assert_eq!(rec["engine"], json!("ants"));
        assert_eq!(rec["custom"], json!(false));
        assert_eq!(rec["version"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(rec["channels"], json!(["T1w", "T2w"]));
        assert_eq!(rec["syn_cc_weights"], json!(["0.5", "0.5"]));
        assert_eq!(rec["t2_source"], path_json(t2_src));
        assert_eq!(
            rec["t2_coreg_transform"],
            path_json(&derivatives::coreg_t2_affine(sub))
        );
        assert_eq!(rec["template"][1], path_json(&case.t2_tpl));
        assert!(rec.get("note").is_none());
        let fwd_affine = derivatives::norm_forward_affine(sub, &space);
        let inv_affine = derivatives::norm_inverse_affine(sub, &space);
        assert_eq!(
            rec["transforms"],
            json!({
                "forward": path_json(&derivatives::norm_forward(sub, &space)),
                "inverse": path_json(&derivatives::norm_inverse(sub, &space)),
                "forward_affine": path_json(&fwd_affine),
                "inverse_affine": path_json(&inv_affine),
            })
        );
        let read_affine = |p: &Path| match fastwarp_reg::transform::read_transform_file(p)
            .unwrap()
            .queue()
        {
            [fastwarp_reg::transform::TransformElem::Affine(a)] => *a,
            other => panic!("{other:?}"),
        };
        assert_eq!(read_affine(&fwd_affine), fake_norm_affine());
        let inverse = read_affine(&inv_affine);
        assert_eq!(inverse, fake_norm_affine().inverse().unwrap());
        let m = &rec["affine_native_to_template_4x4"];
        let native_ras = [10.0, -20.0, 30.0];
        let lps = [-native_ras[0], -native_ras[1], native_ras[2]];
        let t = inverse.apply(lps);
        let want_ras = [-t[0], -t[1], t[2]];
        for r in 0..3 {
            let got = (0..3)
                .map(|c| m[r][c].as_f64().unwrap() * native_ras[c])
                .sum::<f64>()
                + m[r][3].as_f64().unwrap();
            assert!((got - want_ras[r]).abs() < 1e-9, "{rec}");
        }
        assert_eq!(m[3], json!([0.0, 0.0, 0.0, 1.0]));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn multispectral_isotropic_resamples_both_channel_pairs() {
        let mut case = norm_case("msiso", true, false);
        case.opts.norm_isotropic_mm = NormGrid::Mm(2.0);
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        let work = derivatives::norm_work_dir(&case.sub);
        let names = [
            "moving_is2.0mm",
            "template_is2.0mm",
            "moving2_is2.0mm",
            "template2_is2.0mm",
        ]
        .map(|n| work.join(format!("{n}.nii.gz")));
        assert!(!work.exists());
        assert_eq!(calls.len(), 2);
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert!(calls[1].contains(&format!("CC[{},{},0.5,4]", s(&names[1]), s(&names[0]))));
        assert!(calls[1].contains(&format!("CC[{},{},0.5,4]", s(&names[3]), s(&names[2]))));
        let rec = read_norm_channels(&case.sub).unwrap();
        assert_eq!(rec["norm_isotropic_mm"], json!(2.0));
        assert_eq!(rec["norm_grid_source"], json!("explicit"));
        assert!(rec["norm_grid_note"].is_null());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn without_t2_normalize_is_unchanged_t1_only_and_says_why() {
        let case = norm_case("t1only", false, false);
        let (res, calls) = run_fake(&case, false);
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let norm_pfx = norm_work_prefix(&case.sub);
        assert_eq!(
            res.unwrap(),
            StageOutcome::done(format!(
                "ok:normalization:ants_syn:{}",
                image_token(&derivatives::norm_anchor_image(&case.sub, &space))
            ))
        );
        assert_eq!(
            calls,
            vec![build_ants_syn_argv(
                Path::new("antsRegistration"),
                case.sub.anchor_best().unwrap(),
                &case.t1_tpl,
                &norm_pfx,
                &[],
            )]
        );
        let rec = read_norm_channels(&case.sub).unwrap();
        assert_eq!(rec["method"], json!("ants_syn"));
        assert_eq!(rec["channels"], json!(["T1w"]));
        assert_eq!(rec["norm_isotropic_mm"], json!(1.0));
        assert_eq!(rec["norm_grid_source"], json!("template"));
        assert!(rec["t2_source"].is_null());
        assert!(
            rec["note"].as_str().unwrap().contains("no pre-op T2w"),
            "{rec}"
        );
        assert!(!case.sub.coreg_dir.exists());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn multispectral_off_or_no_t2_template_falls_back_to_t1() {
        let mut case = norm_case("off", true, false);
        case.opts.norm_multispectral = false;
        assert_eq!(
            plan_norm_channels(&case.sub, &case.opts).unwrap(),
            NormChannels::T1Only(
                "multispectral normalization disabled (--no-norm-multispectral)".into()
            )
        );
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].iter().any(|a| a.contains(",0.5,4]")));
        case.opts.norm_multispectral = true;
        case.opts.normalization_template_t2 = None;
        match plan_norm_channels(&case.sub, &case.opts).unwrap() {
            NormChannels::T1Only(r) => assert!(r.starts_with("no T2 template"), "{r}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn an_anchor_only_rerun_removes_the_t2_coregistration_of_an_earlier_run() {
        let mut case = norm_case("t2stale", true, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let t2_image = derivatives::coreg_t2_image(&case.sub);
        let t2_affine = derivatives::coreg_t2_affine(&case.sub);
        assert!(t2_image.is_file() && t2_affine.is_file());
        case.opts.norm_multispectral = false;
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!t2_image.exists() && !t2_affine.exists());
        let mut fresh = case.sub.clone();
        fresh.refresh_bundles();
        let slides = crate::checkreg::coreg_slide_specs(&fresh, &case.sub.log_dir.join("m.json"));
        assert!(slides.iter().all(|s| s.modality != "T2w"), "{slides:?}");
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn missing_explicit_t2_template_fails_before_any_tool_runs() {
        let mut case = norm_case("t2missing", true, false);
        case.opts.normalization_template_t2 = Some(case.root.join("nope_t2.nii"));
        let (res, calls) = run_fake(&case, false);
        let e = res.unwrap_err().to_string();
        assert!(
            e.contains("nope_t2.nii") && e.contains("never replaced"),
            "{e}"
        );
        assert!(calls.is_empty());
        assert!(!derivatives::norm_method_log(&case.sub).exists());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn failed_syn_discards_the_t2_coregistration_too() {
        let case = norm_case("synfail", true, false);
        let (res, calls) = run_fake(&case, true);
        assert!(res.is_err());
        assert_eq!(calls.len(), 2);
        assert!(!derivatives::coreg_t2_image(&case.sub).exists());
        assert!(!derivatives::coreg_t2_affine(&case.sub).exists());
        assert!(!derivatives::norm_method_log(&case.sub).exists());
        assert!(
            no_partials(&case.sub.coreg_dir)
                && no_partials(&case.sub.norm_dir)
                && no_partials(&derivatives::coreg_transform_dir(&case.sub))
                && no_partials(&derivatives::norm_transform_dir(&case.sub))
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_ants_outputs_match_only_an_ants_request() {
        let case = norm_case("method_match", false, false);
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        let path = derivatives::norm_method_log(&case.sub);
        let mut rec: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        rec["method"] = json!("other_method");
        std::fs::write(&path, rec.to_string()).unwrap();
        let e = approved_outputs_match(&case.sub, &case.opts).unwrap_err();
        assert!(
            e.contains("made with other_method") && e.contains("ants_syn"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_normalize_reruns_when_the_record_was_made_by_another_method() {
        let case = norm_case("method_record", false, false);
        std::fs::create_dir_all(derivatives::norm_log_dir(&case.sub)).unwrap();
        std::fs::write(
            derivatives::norm_method_log(&case.sub),
            json!({
                "method": "fireants",
                "channels": ["T1w"],
                "template": [path_json(&case.t1_tpl)],
                "norm_isotropic_mm": 1.0,
            })
            .to_string(),
        )
        .unwrap();
        let e = approved_outputs_match(&case.sub, &case.opts).unwrap_err();
        assert!(
            e.contains("made with fireants") && e.contains("ants_syn"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    fn strip_record_keys(case: &NormCase, keys: &[&str]) {
        let path = derivatives::norm_method_log(&case.sub);
        let mut rec: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for k in keys {
            rec.as_object_mut().unwrap().remove(*k);
        }
        std::fs::write(&path, serde_json::to_string(&rec).unwrap()).unwrap();
    }

    #[test]
    fn approved_outputs_follow_the_registration_grid() {
        let case = norm_case("match_grid", false, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let sub = &case.sub;
        let mut o = case.opts.clone();
        o.norm_isotropic_mm = NormGrid::Mm(1.0 + 1e-9);
        approved_outputs_match(sub, &o).unwrap();
        o.norm_isotropic_mm = NormGrid::Mm(1.5);
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert_eq!(
            e,
            "the approved outputs were made on a 1.0 mm grid, this run asks for a 1.5 mm grid"
        );
        o.norm_isotropic_mm = NormGrid::Full;
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert!(
            e.contains("a 1.0 mm grid") && e.contains("the full template grid"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn a_grid_that_was_not_chosen_keeps_approved_outputs_on_any_grid() {
        let case = norm_case("match_unchosen", false, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let path = derivatives::norm_method_log(&case.sub);
        for recorded in [json!(0.5), json!(1.5), Value::Null] {
            let mut rec: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            rec["norm_isotropic_mm"] = recorded;
            std::fs::write(&path, rec.to_string()).unwrap();
            let mut o = case.opts.clone();
            o.norm_isotropic_mm = NormGrid::Template;
            approved_outputs_match(&case.sub, &o).unwrap();
        }
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_full_grid_outputs_do_not_match_a_millimetre_request() {
        let mut case = norm_case("match_full", false, false);
        case.opts.norm_isotropic_mm = NormGrid::Full;
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        let mut o = case.opts.clone();
        o.norm_isotropic_mm = NormGrid::Mm(1.0);
        let e = approved_outputs_match(&case.sub, &o).unwrap_err();
        assert!(
            e.contains("the full template grid") && e.contains("a 1.0 mm grid"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_outputs_follow_the_template() {
        let case = norm_case("match_tpl", false, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let sub = &case.sub;
        let same_stem = case.root.join("tpl_other/t1.nii.gz");
        nii(&same_stem);
        let mut o = case.opts.clone();
        o.normalization_template = Some(absolute(&same_stem));
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert!(
            e.contains("made for the template") && e.contains("tpl_other"),
            "{e}"
        );
        let other_stem = case.root.join("tpl/other.nii.gz");
        nii(&other_stem);
        o.normalization_template = Some(absolute(&other_stem));
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert!(
            e.contains("made for the template")
                && e.contains(&absolute(&case.t1_tpl).display().to_string())
                && e.contains("other.nii.gz"),
            "{e}"
        );
        approved_outputs_match(sub, &case.opts).unwrap();
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_outputs_follow_the_template_space() {
        let case = norm_case("match_space", false, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let path = derivatives::norm_method_log(&case.sub);
        let mut rec: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let want = crate::template_catalog::infer_space(&case.t1_tpl);
        assert_eq!(rec["template_space"], json!(want));
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        rec["template_space"] = json!("SomeOtherSpace");
        std::fs::write(&path, serde_json::to_string(&rec).unwrap()).unwrap();
        let e = approved_outputs_match(&case.sub, &case.opts).unwrap_err();
        assert!(
            e.contains("template space SomeOtherSpace") && e.contains(&want),
            "{e}"
        );
        strip_record_keys(&case, &["template_space"]);
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_outputs_without_any_record_are_kept() {
        let case = norm_case("match_norec", false, false);
        let mut o = case.opts.clone();
        o.norm_isotropic_mm = NormGrid::Mm(2.0);
        approved_outputs_match(&case.sub, &o).unwrap();
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_outputs_with_a_legacy_record_are_kept() {
        let case = norm_case("match_legacy", true, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        strip_record_keys(&case, &["norm_isotropic_mm", "template", "channels"]);
        let mut o = case.opts.clone();
        o.norm_isotropic_mm = NormGrid::Mm(2.0);
        o.norm_multispectral = false;
        approved_outputs_match(&case.sub, &o).unwrap();
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn approved_outputs_follow_the_multispectral_mode_and_t2_template() {
        let case = norm_case("match_ms", true, false);
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        let sub = &case.sub;
        approved_outputs_match(sub, &case.opts).unwrap();
        let mut o = case.opts.clone();
        o.norm_multispectral = false;
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert!(
            e.contains("from T1w + T2w") && e.contains("anchor image only"),
            "{e}"
        );
        let other_t2 = case.root.join("tpl_t2/t2.nii.gz");
        nii(&other_t2);
        let mut o = case.opts.clone();
        o.normalization_template_t2 = Some(absolute(&other_t2));
        let e = approved_outputs_match(sub, &o).unwrap_err();
        assert!(e.contains("T2 template") && e.contains("tpl_t2"), "{e}");
        let _ = std::fs::remove_dir_all(&case.root);

        let mut case = norm_case("match_ms_off", true, false);
        case.opts.norm_multispectral = false;
        let (res, _) = run_fake(&case, false);
        res.unwrap();
        approved_outputs_match(&case.sub, &case.opts).unwrap();
        let mut o = case.opts.clone();
        o.norm_multispectral = true;
        let e = approved_outputs_match(&case.sub, &o).unwrap_err();
        assert!(
            e.contains("anchor image only") && e.contains("T1w + T2w"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn empty_warped_ct_is_detected() {
        let (root, _sub) = scratch("empty");
        let p = root.join("w.nii.gz");
        crate::nifti_io::write_u8_3d(
            &p,
            &ndarray::Array3::<u8>::zeros((4, 4, 4)),
            &[
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        )
        .unwrap();
        assert!(warped_is_empty(&p).unwrap());
        let mut d = ndarray::Array3::<u8>::zeros((4, 4, 4));
        d[[1, 1, 1]] = 7;
        crate::nifti_io::write_u8_3d(
            &p,
            &d,
            &[
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        )
        .unwrap();
        assert!(!warped_is_empty(&p).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn blob(p: &Path) {
        use fastwarp_reg::{Grid, Volume};
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let shift = if p.to_string_lossy().contains("tpl") {
            0.0
        } else {
            1.5
        };
        let n = 24usize;
        let g = Grid::new(
            [n, n, n],
            [1.0; 3],
            [0.0; 3],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        let centres = [
            ([0.0, 0.0, 0.0], 5.0, 0.8),
            ([4.0, 2.0, -1.0], 2.5, 0.9),
            ([-4.0, 3.0, 2.0], 3.0, 0.7),
        ];
        let data: Vec<f32> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let c = (n as f64 - 1.0) / 2.0;
                let (x, y, z) = (i as f64 - c - shift, j as f64 - c, k as f64 - c);
                let noise = ((lin * 2_654_435_761) % 1000) as f64 / 20_000.0;
                let v: f64 = centres
                    .iter()
                    .map(|(b, s, a)| {
                        let d2 = (x - b[0]).powi(2) + (y - b[1]).powi(2) + (z - b[2]).powi(2);
                        a * (-d2 / (2.0 * s * s)).exp()
                    })
                    .sum();
                (100.0 * (v + noise)) as f32
            })
            .collect();
        Volume::from_vec(g, data).unwrap().save_nifti(p).unwrap();
    }

    struct FailingEngine;

    impl crate::reg_backend::RegEngine for FailingEngine {
        fn describe(&self) -> Value {
            json!({"name": "failing"})
        }

        fn label(&self) -> String {
            "Failing test engine (test, Cpu)".to_owned()
        }

        fn register(
            &self,
            _: &fastwarp_reg::register::LinearPlan,
            _: &fastwarp_reg::recipe::SynRecipe,
            _: &[fastwarp_reg::Volume<f64>],
            _: &[fastwarp_reg::Volume<f64>],
            _: Option<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::register::RegisterOutput> {
            Err(fastwarp_reg::RegError::Invalid {
                what: "test",
                message: "out of memory in the test engine".into(),
            })
        }

        fn rigid(
            &self,
            recipe: &fastwarp_reg::linear::LinearRecipe,
            fixed: &fastwarp_reg::Volume<f64>,
            moving: &fastwarp_reg::Volume<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            <crate::reg_backend::CpuReferenceEngine as crate::reg_backend::RegEngine>::rigid(
                &crate::reg_backend::CpuReferenceEngine,
                recipe,
                fixed,
                moving,
            )
        }

        fn rigid_from(
            &self,
            recipe: &fastwarp_reg::linear::LinearRecipe,
            fixed: &fastwarp_reg::Volume<f64>,
            moving: &fastwarp_reg::Volume<f64>,
            initial: fastwarp_reg::AffineTransform,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            <crate::reg_backend::CpuReferenceEngine as crate::reg_backend::RegEngine>::rigid_from(
                &crate::reg_backend::CpuReferenceEngine,
                recipe,
                fixed,
                moving,
                initial,
            )
        }
    }

    fn native_tools(engine: Box<dyn crate::reg_backend::RegEngine>) -> NormTools<'static> {
        NormTools {
            ants: None,
            apply: None,
            reg: RegChoice {
                requested: RegBackend::Auto,
                resolution: Resolution::Wgpu,
                engine: Some(engine),
            },
            cancel: None,
        }
    }

    fn never_exec(_: &[String]) -> PipelineResult<()> {
        panic!("no external program may run on the wgpu backend");
    }

    fn read_record(case: &NormCase) -> Value {
        let text = std::fs::read_to_string(derivatives::norm_method_log(&case.sub)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn native_normalize_writes_the_ants_named_outputs_and_the_record() {
        let mut case = norm_case_with("native", true, true, blob);
        case.opts.norm_isotropic_mm = NormGrid::Mm(1.5);
        let work = derivatives::norm_work_dir(&case.sub);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("template_is1.5mm.nii.gz"), b"stale").unwrap();
        let tools = native_tools(Box::new(crate::reg_backend::CpuReferenceEngine));
        let mut exec = never_exec;
        let out = normalize_with(&case.sub, &case.opts, &tools, &mut exec, None).unwrap();
        assert!(matches!(out, StageOutcome::Completed(_)));
        let sub = &case.sub;
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        assert!(!work.exists());
        let ct_out = derivatives::norm_postop_image(sub, "CT", &space);
        for f in [
            derivatives::norm_anchor_image(sub, &space),
            derivatives::norm_forward(sub, &space),
            derivatives::norm_inverse(sub, &space),
            ct_out.clone(),
            derivatives::coreg_t2_affine(sub),
            derivatives::coreg_t2_image(sub),
            derivatives::norm_method_log(&case.sub),
        ] {
            assert!(f.is_file(), "missing {}", f.display());
        }
        assert!(no_partials(&sub.norm_dir));
        assert!(no_partials(&sub.coreg_dir));
        assert!(no_partials(&derivatives::norm_transform_dir(sub)));
        assert!(no_partials(&derivatives::coreg_transform_dir(sub)));
        let pair = fastwarp_reg::transform::CompositePair {
            forward: read_transform_file(&derivatives::norm_forward(sub, &space)).unwrap(),
            inverse: read_transform_file(&derivatives::norm_inverse(sub, &space)).unwrap(),
        };
        assert_eq!((pair.forward.len(), pair.inverse.len()), (2, 2));
        let ct = fastwarp_reg::Volume::<f32>::load_nifti(&ct_out).unwrap();
        let tpl = fastwarp_reg::nifti_io::read_grid(&case.t1_tpl).unwrap();
        assert!(ct.grid().approx_eq(&tpl, 1e-6));
        let src = fastwarp_reg::Volume::<f32>::load_nifti(&derivatives::coreg_ct_image(sub))
            .unwrap()
            .to_f64();
        let want = fastwarp_reg::apply::apply_chain(
            &src,
            &tpl,
            &pair.forward,
            fastwarp_reg::apply::Interpolation::Linear,
            0.0,
        )
        .unwrap();
        for (a, b) in ct.data().iter().zip(want.data()) {
            assert!((f64::from(*a) - b).abs() < 1e-3);
        }
        let rec = read_record(&case);
        assert_eq!(rec["method"], json!("ants_syn"));
        assert_eq!(rec["reg_backend"], json!("wgpu"));
        assert_eq!(rec["reg_requested"], json!("auto"));
        assert_eq!(rec["reg_adapter"]["name"], json!("CPU reference"));
        assert_eq!(rec["reg_t2_coreg"], json!("wgpu"));
        assert_eq!(rec["channels"], json!(["T1w", "T2w"]));
        for key in [
            "reg_timings",
            "reg_syn_levels",
            "reg_rigid_levels",
            "reg_affine_levels",
            "reg_affine_parameters",
            "reg_t2_coreg_levels",
        ] {
            assert!(rec.get(key).is_some(), "record lacks {key}: {rec}");
        }
        assert!(rec["reg_timings"]["total_s"].as_f64().unwrap() > 0.0);
        let fwd_affine = derivatives::norm_forward_affine(sub, &space);
        let inv_affine = derivatives::norm_inverse_affine(sub, &space);
        let linear = match pair.forward.queue() {
            [fastwarp_reg::transform::TransformElem::Affine(a), _] => *a,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            read_transform_file(&fwd_affine).unwrap().queue(),
            &[fastwarp_reg::transform::TransformElem::Affine(linear)]
        );
        assert_eq!(
            read_transform_file(&inv_affine).unwrap().queue(),
            &[fastwarp_reg::transform::TransformElem::Affine(
                linear.inverse().unwrap()
            )]
        );
        assert_eq!(rec["transforms"]["forward_affine"], path_json(&fwd_affine));
        assert_eq!(rec["transforms"]["inverse_affine"], path_json(&inv_affine));
        assert_eq!(
            rec["affine_native_to_template_4x4"]
                .as_array()
                .map(Vec::len),
            Some(4)
        );
        std::fs::remove_file(&fwd_affine).unwrap();
        let sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let written = backfill_norm_affines(&sub).unwrap();
        assert_eq!(written, vec![fwd_affine.clone(), inv_affine.clone()]);
        assert_eq!(
            read_transform_file(&fwd_affine).unwrap().queue(),
            &[fastwarp_reg::transform::TransformElem::Affine(linear)]
        );
        assert!(backfill_norm_affines(&sub).unwrap().is_empty());
        assert!(no_partials(&derivatives::norm_transform_dir(&sub)));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn native_normalize_removes_stale_resampled_files_when_no_resampling_is_needed() {
        let case = norm_case_with("native_noop", false, false, blob);
        let work = derivatives::norm_work_dir(&case.sub);
        std::fs::create_dir_all(&work).unwrap();
        let stale = work.join("template_is1.0mm.nii.gz");
        std::fs::write(&stale, b"stale").unwrap();
        let tools = native_tools(Box::new(crate::reg_backend::CpuReferenceEngine));
        let mut exec = never_exec;
        normalize_with(&case.sub, &case.opts, &tools, &mut exec, None).unwrap();
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn native_normalize_without_t2_or_ct_registers_the_anchor_only() {
        let case = norm_case_with("native_t1", false, false, blob);
        let tools = native_tools(Box::new(crate::reg_backend::CpuReferenceEngine));
        let mut exec = never_exec;
        normalize_with(&case.sub, &case.opts, &tools, &mut exec, None).unwrap();
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        assert!(derivatives::norm_inverse(&case.sub, &space).is_file());
        assert!(!derivatives::norm_postop_image(&case.sub, "CT", &space).exists());
        let rec = read_record(&case);
        assert_eq!(rec["channels"], json!(["T1w"]));
        assert_eq!(rec["reg_t2_coreg"], Value::Null);
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn a_failing_native_registration_keeps_the_previous_outputs_and_names_the_adapter() {
        let case = norm_case_with("native_fail", false, false, blob);
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let comp = derivatives::norm_forward(&case.sub, &space);
        std::fs::create_dir_all(comp.parent().unwrap()).unwrap();
        std::fs::write(&comp, b"previous").unwrap();
        let tools = native_tools(Box::new(FailingEngine));
        let mut exec = never_exec;
        let e = normalize_with(&case.sub, &case.opts, &tools, &mut exec, None).unwrap_err();
        let text = e.to_string();
        assert!(
            text.contains("Failing test engine") && text.contains("--reg-backend ants"),
            "{text}"
        );
        assert_eq!(std::fs::read(&comp).unwrap(), b"previous");
        assert!(no_partials(&case.sub.norm_dir));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn a_set_cancel_flag_stops_the_native_normalize_before_it_starts() {
        let case = norm_case_with("native_cancel", true, false, blob);
        let flag = AtomicBool::new(true);
        let mut tools = native_tools(Box::new(crate::reg_backend::CpuReferenceEngine));
        tools.cancel = Some(&flag);
        let mut exec = never_exec;
        let e = normalize_with(&case.sub, &case.opts, &tools, &mut exec, None).unwrap_err();
        assert!(matches!(e, PipelineError::Cancelled), "{e}");
        assert!(no_partials(&case.sub.norm_dir));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn ants_normalize_records_the_backend_it_used() {
        let case = norm_case("ants_record", false, false);
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 1);
        let rec = read_record(&case);
        assert_eq!(rec["reg_backend"], json!("ants"));
        assert_eq!(rec["reg_requested"], json!("ants"));
        assert!(rec.get("reg_adapter").is_none());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    fn write_backend_record(case: &NormCase, backend: Option<&str>) {
        std::fs::create_dir_all(derivatives::norm_log_dir(&case.sub)).unwrap();
        let mut rec = json!({
            "method": "ants_syn",
            "channels": ["T1w"],
            "template": [absolute(&case.t1_tpl).to_string_lossy()],
            "norm_isotropic_mm": 1.0,
        });
        if let Some(b) = backend {
            rec["reg_backend"] = json!(b);
        }
        std::fs::write(derivatives::norm_method_log(&case.sub), rec.to_string()).unwrap();
    }

    #[test]
    fn approved_outputs_follow_the_registration_backend_rule() {
        let case = norm_case("lock_backend", false, false);
        let with = |b: RegBackend| RunOptions {
            reg_backend: b,
            ..case.opts.clone()
        };
        for recorded in [None, Some("ants"), Some("wgpu")] {
            write_backend_record(&case, recorded);
            approved_outputs_match(&case.sub, &with(RegBackend::Auto)).unwrap();
            let have = recorded.unwrap_or("ants");
            for asked in [RegBackend::Ants, RegBackend::Wgpu] {
                let r = approved_outputs_match(&case.sub, &with(asked));
                if asked.as_str() == have {
                    r.unwrap();
                } else {
                    let e = r.unwrap_err();
                    assert_eq!(
                        e,
                        format!(
                            "the approved outputs were made with the {have} registration \
                             backend, this run asks for {}",
                            asked.as_str()
                        )
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn the_backend_rule_ignores_a_missing_record() {
        let case = norm_case("lock_backend_none", false, false);
        let o = RunOptions {
            reg_backend: RegBackend::Wgpu,
            ..case.opts.clone()
        };
        approved_outputs_match(&case.sub, &o).unwrap();
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn the_point_warp_route_follows_the_backend_request_and_the_record() {
        let case = norm_case("warp_route", false, false);
        let with = |b: RegBackend| RunOptions {
            reg_backend: b,
            ..case.opts.clone()
        };
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Ants)),
            WarpRoute::Ants
        );
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Auto)),
            WarpRoute::Auto
        );
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Wgpu)),
            WarpRoute::Auto
        );
        write_backend_record(&case, Some("ants"));
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Auto)),
            WarpRoute::Auto
        );
        write_backend_record(&case, Some("wgpu"));
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Auto)),
            WarpRoute::Rust
        );
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Wgpu)),
            WarpRoute::Rust
        );
        assert_eq!(
            warp_route_for(&case.sub, &with(RegBackend::Ants)),
            WarpRoute::Ants
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    fn write_coreg_record(sub: &Subject, backend: Option<&str>) {
        let record = derivatives::coreg_multistart_record(sub);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        let mut rec = json!({
            "starts": [],
            "chosen_pitch_deg": 0.0,
            "fixed_centroid_lps": [0.0, 0.0, 0.0],
            "moving_centroid_lps": [0.0, 0.0, 0.0],
            "coarse_schedule": {},
            "fixed_mask": "mask.nii.gz",
        });
        if let Some(b) = backend {
            rec["reg_backend"] = json!(b);
        }
        std::fs::write(&record, rec.to_string()).unwrap();
    }

    #[test]
    fn coreg_approved_outputs_follow_the_registration_backend_rule() {
        let (root, sub) = scratch("coreg_lock_backend");
        let with = |b: RegBackend| RunOptions {
            reg_backend: b,
            ..RunOptions::new(&root, "1")
        };
        for recorded in [None, Some("ants"), Some("wgpu")] {
            write_coreg_record(&sub, recorded);
            coreg_approved_outputs_match(&sub, &with(RegBackend::Auto)).unwrap();
            let have = recorded.unwrap_or("ants");
            for asked in [RegBackend::Ants, RegBackend::Wgpu] {
                let r = coreg_approved_outputs_match(&sub, &with(asked));
                if asked.as_str() == have {
                    r.unwrap();
                } else {
                    let e = r.unwrap_err();
                    assert_eq!(
                        e,
                        format!(
                            "the approved outputs were made with the {have} registration \
                             backend, this run asks for {}",
                            asked.as_str()
                        )
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_coreg_backend_rule_ignores_a_missing_record() {
        let (root, sub) = scratch("coreg_lock_none");
        let o = RunOptions {
            reg_backend: RegBackend::Wgpu,
            ..RunOptions::new(&root, "1")
        };
        coreg_approved_outputs_match(&sub, &o).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    fn reg_blob(path: &Path, shift: f64) {
        use fastwarp_reg::{Grid, Volume};
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let n = 24usize;
        let g = Grid::new(
            [n, n, n],
            [1.0; 3],
            [0.0; 3],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        let data: Vec<f32> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let c = (n as f64 - 1.0) / 2.0;
                let (x, y, z) = (i as f64 - c - shift, j as f64 - c, k as f64 - c);
                let noise = ((lin * 2_654_435_761) % 1000) as f64 / 20_000.0;
                let v = (-(x * x + y * y + z * z) / (2.0 * 6.0 * 6.0)).exp()
                    + 0.6 * (-((x - 5.0).powi(2) + (y + 3.0).powi(2) + z * z) / 8.0).exp();
                (100.0 * (v + noise)) as f32
            })
            .collect();
        Volume::from_vec(g, data).unwrap().save_nifti(path).unwrap();
    }

    #[test]
    fn the_wgpu_coregister_stage_registers_a_small_ct_pair_when_an_adapter_exists() {
        let Ok(wgpu) = crate::reg_backend::WgpuEngine::create() else {
            return;
        };
        let root = std::env::temp_dir().join(format!("dbsloc_coreg_gpu_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fixed = root.join("anchor.nii.gz");
        let moving = root.join("ct.nii.gz");
        reg_blob(&fixed, 0.0);
        reg_blob(&moving, 3.0);

        let dir = root.join("coregistration");
        let tdir = dir.join("transformations");
        for d in [dir.join("anat"), dir.join("log"), tdir.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        let paths = CtPaths {
            warped: dir.join("anat").join("ct_in_anchor.nii.gz"),
            record: dir.join("log").join("ct_multistart.json"),
            work_dir: dir.join("log").join("multistart"),
        };
        let warped = paths.warped.clone();
        let record = paths.record.clone();
        let affine = tdir.join("ct_to_anchor.mat");
        let forward = tdir.join("ct_to_anchor.h5");
        let inverse = tdir.join("anchor_to_ct.h5");
        let tmp_prefix = derivatives::work_prefix(&tdir, "sub-1_CT");
        let staged = Staged::mapped(vec![
            (warped.clone(), suffixed(&tmp_prefix, ANTS_WARPED_SUFFIX)),
            (affine.clone(), suffixed(&tmp_prefix, ANTS_AFFINE_SUFFIX)),
            (record.clone(), crate::outputs::partial_of(&record)),
            (
                forward.clone(),
                suffixed(&tmp_prefix, ANTS_COMPOSITE_SUFFIX),
            ),
            (
                inverse.clone(),
                suffixed(&tmp_prefix, ANTS_INVERSE_COMPOSITE_SUFFIX),
            ),
        ])
        .unwrap();
        let reg = RegChoice {
            requested: RegBackend::Wgpu,
            resolution: Resolution::Wgpu,
            engine: Some(Box::new(wgpu)),
        };
        let engine: &dyn RegEngine = reg.engine.as_deref().unwrap();
        let coarse = LinearRecipe {
            iterations: vec![20, 10],
            shrink_factors: vec![4, 2],
            smoothing_sigmas: vec![2.0, 1.0],
            cache_smoothed_images: false,
            ..LinearRecipe::cohort_rigid()
        };
        let full = LinearRecipe {
            iterations: vec![30, 15],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            cache_smoothed_images: false,
            ..LinearRecipe::cohort_rigid()
        };
        coregister_native_with(
            engine,
            &reg,
            &moving,
            &fixed,
            &paths,
            &staged,
            &tmp_prefix,
            &coarse,
            &full,
            None,
            None,
        )
        .unwrap();
        staged.commit(None).unwrap();

        for f in [
            warped.clone(),
            affine.clone(),
            forward.clone(),
            inverse.clone(),
            record.clone(),
        ] {
            assert!(f.is_file(), "missing {}", f.display());
        }
        let rec: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(rec["reg_backend"], json!("wgpu"));
        assert_eq!(rec["reg_requested"], json!("wgpu"));
        assert!(rec["reg_adapter"]["name"].is_string(), "{rec}");
        assert_eq!(rec["starts"][0]["metric_backend"], json!("wgpu"));
        assert!(rec["reg_timings"]["coarse"].as_array().unwrap().len() == PITCH_STARTS_DEG.len());
        assert!(!rec["reg_timings"]["full_levels"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(rec["reg_timings"]["full_s"].as_f64().unwrap() > 0.0);
        assert!(rec.get("reg_note").is_none());
        assert!(no_partials(&tdir));
        assert!(no_partials(&dir.join("anat")));
        assert!(no_partials(&dir.join("log")));
        assert!(!warped_is_empty(&warped).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn preprocess_case(name: &str) -> (PathBuf, Subject) {
        let root = std::env::temp_dir().join(format!("dbsloc_pre_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let anat = root.join("rawdata/sub-1/ses-preop/anat");
        blob(&anat.join("sub-1_ses-preop_T1w.nii.gz"));
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        (root, sub)
    }

    fn preprocess_tools(engine: Box<dyn crate::reg_backend::N4Engine>) -> PreprocessTools<'static> {
        PreprocessTools {
            n4: None,
            choice: crate::reg_backend::N4Choice {
                requested: RegBackend::Wgpu,
                resolution: Resolution::Wgpu,
                engine: Some(engine),
            },
            threads: None,
            cancel: None,
        }
    }

    fn n4_paths(sub: &Subject) -> (PathBuf, PathBuf) {
        (
            derivatives::preproc_image(sub),
            derivatives::preproc_record(sub),
        )
    }

    fn read_n4_record(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn preprocess_in_process_stages_the_n4_output_and_record() {
        let (root, sub) = preprocess_case("cpu");
        let tools = preprocess_tools(Box::new(crate::reg_backend::CpuN4Engine));
        let mut exec = never_exec;
        let out = preprocess_with(&sub, &tools, &mut exec, None).unwrap();
        assert!(matches!(out, StageOutcome::Completed(_)));
        let (nii, rec_path) = n4_paths(&sub);
        assert!(nii.is_file() && rec_path.is_file());
        assert!(no_partials(&sub.preproc_dir));

        let rec = read_n4_record(&rec_path);
        assert_eq!(rec["reg_backend"], json!("wgpu"));
        assert_eq!(rec["reg_requested"], json!("wgpu"));
        assert_eq!(rec["reg_adapter"]["name"], json!("CPU reference"));
        assert!(rec.get("reg_note").is_none());
        assert_eq!(rec["n4"]["shrink_factor"], json!(4));
        assert_eq!(rec["n4"]["fitting_levels"], json!(4));
        assert_eq!(rec["n4"]["spline_order"], json!(3));
        assert_eq!(rec["n4"]["number_of_control_points"], json!([4, 4, 4]));
        assert_eq!(rec["n4"]["number_of_histogram_bins"], json!(200));
        assert_eq!(rec["n4"]["convergence_threshold"], json!(0.0));
        let levels = rec["levels"].as_array().unwrap();
        assert_eq!(levels.len(), 4);
        for l in levels {
            assert_eq!(l["iterations"], json!(50));
            assert_eq!(l["convergence"].as_array().unwrap().len(), 50);
        }
        assert!(rec["reg_timings"]["total_s"].as_f64().unwrap() >= 0.0);
        assert!(rec["reg_timings"]["n4_s"].as_f64().unwrap() >= 0.0);

        let corrected = Volume::<f32>::load_nifti(&nii).unwrap();
        assert!(corrected.data().iter().all(|v| v.is_finite() && *v > 0.0));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_preprocess_output_removes_the_precoreg_image_built_from_the_old_one() {
        let (root, sub) = preprocess_case("stale_precoreg");
        let image = derivatives::precoreg_image(&sub);
        let record = derivatives::precoreg_record(&sub);
        for p in [&image, &record] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"old").unwrap();
        }
        let (probe, _) = ThreadProbe::new();
        let tools = preprocess_tools(Box::new(probe));
        let mut exec = never_exec;
        assert!(preprocess_with(&sub, &tools, &mut exec, None).is_err());
        assert!(image.is_file() && record.is_file());
        let tools = preprocess_tools(Box::new(crate::reg_backend::CpuN4Engine));
        preprocess_with(&sub, &tools, &mut exec, None).unwrap();
        assert!(!image.exists() && !record.exists());
        let mut fresh = sub.clone();
        fresh.refresh_bundles();
        assert_eq!(
            fresh.anchor_in_use(),
            Some(derivatives::preproc_image(&sub))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preprocess_runs_on_the_gpu_n4_when_an_adapter_exists() {
        let Ok(engine) = crate::reg_backend::WgpuN4Engine::create() else {
            return;
        };
        let (root, sub) = preprocess_case("gpu");
        let tools = preprocess_tools(Box::new(engine));
        let mut exec = never_exec;
        preprocess_with(&sub, &tools, &mut exec, None).unwrap();
        let (nii, rec_path) = n4_paths(&sub);
        assert!(nii.is_file() && rec_path.is_file());
        assert!(no_partials(&sub.preproc_dir));
        let rec = read_n4_record(&rec_path);
        assert_eq!(rec["reg_backend"], json!("wgpu"));
        assert_eq!(rec["reg_requested"], json!("wgpu"));
        assert!(rec["reg_adapter"]["name"].is_string(), "{rec}");
        assert_eq!(rec["levels"].as_array().unwrap().len(), 4);
        let corrected = Volume::<f32>::load_nifti(&nii).unwrap();
        assert!(corrected.data().iter().all(|v| v.is_finite()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_preprocess_lock_follows_the_registration_backend_rule() {
        let (root, sub) = scratch("pre_lock");
        let (_, rec_path) = n4_paths(&sub);
        std::fs::create_dir_all(&sub.preproc_dir).unwrap();
        let opts = |backend: RegBackend| RunOptions {
            reg_backend: backend,
            ..RunOptions::new(&root, "1")
        };

        assert!(preprocess_approved_outputs_match(&sub, &opts(RegBackend::Auto)).is_ok());
        assert!(preprocess_approved_outputs_match(&sub, &opts(RegBackend::Ants)).is_ok());
        let e = preprocess_approved_outputs_match(&sub, &opts(RegBackend::Wgpu)).unwrap_err();
        assert!(
            e.contains("made with the ants registration backend")
                && e.contains("this run asks for wgpu"),
            "{e}"
        );

        std::fs::write(&rec_path, json!({"reg_backend": "wgpu"}).to_string()).unwrap();
        assert!(preprocess_approved_outputs_match(&sub, &opts(RegBackend::Auto)).is_ok());
        assert!(preprocess_approved_outputs_match(&sub, &opts(RegBackend::Wgpu)).is_ok());
        let e = preprocess_approved_outputs_match(&sub, &opts(RegBackend::Ants)).unwrap_err();
        assert!(e.contains("made with the wgpu registration backend"), "{e}");

        std::fs::write(&rec_path, json!({"n4": {}}).to_string()).unwrap();
        let e = preprocess_approved_outputs_match(&sub, &opts(RegBackend::Wgpu)).unwrap_err();
        assert!(e.contains("made with the ants registration backend"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    struct ThreadProbe(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ThreadProbe {
        fn new() -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
            let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (Self(seen.clone()), seen)
        }

        fn record(&self) -> fastwarp_reg::RegError {
            self.0.store(rayon::current_num_threads(), Ordering::SeqCst);
            fastwarp_reg::RegError::Invalid {
                what: "test",
                message: "thread probe".into(),
            }
        }
    }

    impl crate::reg_backend::RegEngine for ThreadProbe {
        fn describe(&self) -> Value {
            json!({"name": "thread probe"})
        }

        fn label(&self) -> String {
            "Thread probe (test, Cpu)".to_owned()
        }

        fn register(
            &self,
            _: &fastwarp_reg::register::LinearPlan,
            _: &fastwarp_reg::recipe::SynRecipe,
            _: &[fastwarp_reg::Volume<f64>],
            _: &[fastwarp_reg::Volume<f64>],
            _: Option<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::register::RegisterOutput> {
            Err(self.record())
        }

        fn rigid(
            &self,
            _: &fastwarp_reg::linear::LinearRecipe,
            _: &fastwarp_reg::Volume<f64>,
            _: &fastwarp_reg::Volume<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            Err(self.record())
        }

        fn rigid_from(
            &self,
            _: &fastwarp_reg::linear::LinearRecipe,
            _: &fastwarp_reg::Volume<f64>,
            _: &fastwarp_reg::Volume<f64>,
            _: fastwarp_reg::transform::AffineTransform,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            Err(self.record())
        }
    }

    impl crate::reg_backend::N4Engine for ThreadProbe {
        fn describe(&self) -> Value {
            json!({"name": "thread probe"})
        }

        fn label(&self) -> String {
            "Thread probe (test, Cpu)".to_owned()
        }

        fn run_n4(&self, _: &Volume<f64>, _: &N4Params) -> Result<N4Outcome, String> {
            Err(self.record().to_string())
        }
    }

    #[test]
    fn the_in_process_bodies_run_on_ants_max_threads_workers() {
        let case = norm_case_with("threads_norm", false, false, blob);
        let opts = RunOptions {
            ants_max_threads: Some(3),
            ..case.opts.clone()
        };
        let (probe, seen) = ThreadProbe::new();
        let tools = native_tools(Box::new(probe));
        let mut exec = never_exec;
        assert!(normalize_with(&case.sub, &opts, &tools, &mut exec, None).is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 3, "normalize");
        let _ = std::fs::remove_dir_all(&case.root);

        let root =
            std::env::temp_dir().join(format!("dbsloc_coreg_threads_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fixed = root.join("anchor.nii.gz");
        let moving = root.join("ct.nii.gz");
        reg_blob(&fixed, 0.0);
        reg_blob(&moving, 3.0);
        let dir = root.join("coregistration");
        std::fs::create_dir_all(dir.join("log")).unwrap();
        let paths = CtPaths {
            warped: dir.join("ct_in_anchor.nii.gz"),
            record: dir.join("log").join("ct_multistart.json"),
            work_dir: dir.join("log").join("multistart"),
        };
        let staged = Staged::new(vec![paths.warped.clone()]).unwrap();
        let tmp_prefix = crate::outputs::partial_of(&dir.join("ct_in_anchor"));
        let reg = RegChoice {
            requested: RegBackend::Wgpu,
            resolution: Resolution::Wgpu,
            engine: None,
        };
        let (probe, seen) = ThreadProbe::new();
        let r = coregister_native(
            &probe,
            &reg,
            &moving,
            &fixed,
            &paths,
            &staged,
            &tmp_prefix,
            Some(2),
            None,
            None,
        );
        assert!(r.is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 2, "coregister");
        let _ = std::fs::remove_dir_all(&root);

        let (root, sub) = preprocess_case("threads");
        let (probe, seen) = ThreadProbe::new();
        let mut tools = preprocess_tools(Box::new(probe));
        tools.threads = Some(2);
        let mut exec = never_exec;
        assert!(preprocess_with(&sub, &tools, &mut exec, None).is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 2, "preprocess");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cancelled_run_stops_between_the_native_coarse_starts() {
        use crate::reg_backend::{CpuReferenceEngine, RegEngine};
        struct CancelAfterFirst<'a> {
            cancel: &'a AtomicBool,
            calls: std::sync::atomic::AtomicUsize,
        }
        impl RegEngine for CancelAfterFirst<'_> {
            fn describe(&self) -> Value {
                CpuReferenceEngine.describe()
            }
            fn label(&self) -> String {
                CpuReferenceEngine.label()
            }
            fn register(
                &self,
                plan: &fastwarp_reg::register::LinearPlan,
                recipe: &fastwarp_reg::recipe::SynRecipe,
                fixed: &[fastwarp_reg::Volume<f64>],
                moving: &[fastwarp_reg::Volume<f64>],
                iso: Option<f64>,
            ) -> fastwarp_reg::Result<fastwarp_reg::register::RegisterOutput> {
                CpuReferenceEngine.register(plan, recipe, fixed, moving, iso)
            }
            fn rigid(
                &self,
                r: &fastwarp_reg::linear::LinearRecipe,
                f: &fastwarp_reg::Volume<f64>,
                m: &fastwarp_reg::Volume<f64>,
            ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
                CpuReferenceEngine.rigid(r, f, m)
            }
            fn rigid_from(
                &self,
                r: &fastwarp_reg::linear::LinearRecipe,
                f: &fastwarp_reg::Volume<f64>,
                m: &fastwarp_reg::Volume<f64>,
                i: fastwarp_reg::transform::AffineTransform,
            ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.cancel.store(true, Ordering::SeqCst);
                CpuReferenceEngine.rigid_from(r, f, m, i)
            }
        }

        let root = std::env::temp_dir().join(format!("dbsloc_coreg_cancel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fixed = root.join("anchor.nii.gz");
        let moving = root.join("ct.nii.gz");
        reg_blob(&fixed, 0.0);
        reg_blob(&moving, 3.0);
        let dir = root.join("coregistration");
        std::fs::create_dir_all(dir.join("log")).unwrap();
        let paths = CtPaths {
            warped: dir.join("ct_in_anchor.nii.gz"),
            record: dir.join("log").join("ct_multistart.json"),
            work_dir: dir.join("log").join("multistart"),
        };
        let staged = Staged::new(vec![paths.warped.clone()]).unwrap();
        let tmp_prefix = crate::outputs::partial_of(&dir.join("ct_in_anchor"));
        let reg = RegChoice {
            requested: RegBackend::Wgpu,
            resolution: Resolution::Wgpu,
            engine: None,
        };
        let cancel = AtomicBool::new(false);
        let engine = CancelAfterFirst {
            cancel: &cancel,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let r = coregister_native(
            &engine,
            &reg,
            &moving,
            &fixed,
            &paths,
            &staged,
            &tmp_prefix,
            None,
            Some(&cancel),
            None,
        );
        assert!(matches!(r, Err(PipelineError::Cancelled)), "{r:?}");
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    struct CancelInCall {
        masked_only: bool,
    }

    impl crate::reg_backend::RegEngine for CancelInCall {
        fn describe(&self) -> Value {
            json!({"name": "cancel in call"})
        }
        fn label(&self) -> String {
            "Cancel in call test engine (test, Cpu)".to_owned()
        }
        fn register(
            &self,
            plan: &fastwarp_reg::register::LinearPlan,
            recipe: &fastwarp_reg::recipe::SynRecipe,
            fixed: &[fastwarp_reg::Volume<f64>],
            moving: &[fastwarp_reg::Volume<f64>],
            iso: Option<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::register::RegisterOutput> {
            crate::reg_backend::CpuReferenceEngine.register(plan, recipe, fixed, moving, iso)
        }
        fn rigid(
            &self,
            r: &fastwarp_reg::linear::LinearRecipe,
            f: &fastwarp_reg::Volume<f64>,
            m: &fastwarp_reg::Volume<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            crate::reg_backend::CpuReferenceEngine.rigid(r, f, m)
        }
        fn rigid_from(
            &self,
            r: &fastwarp_reg::linear::LinearRecipe,
            f: &fastwarp_reg::Volume<f64>,
            m: &fastwarp_reg::Volume<f64>,
            i: fastwarp_reg::transform::AffineTransform,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            crate::reg_backend::CpuReferenceEngine.rigid_from(r, f, m, i)
        }
        fn rigid_cancellable(
            &self,
            _: &fastwarp_reg::linear::LinearRecipe,
            _: &fastwarp_reg::Volume<f64>,
            _: &fastwarp_reg::Volume<f64>,
            _: Option<&AtomicBool>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            Err(fastwarp_reg::RegError::Cancelled)
        }
        fn rigid_from_cancellable(
            &self,
            r: &fastwarp_reg::linear::LinearRecipe,
            f: &fastwarp_reg::Volume<f64>,
            m: &fastwarp_reg::Volume<f64>,
            i: fastwarp_reg::transform::AffineTransform,
            _: Option<&AtomicBool>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            if self.masked_only && r.mask.is_none() {
                return crate::reg_backend::CpuReferenceEngine.rigid_from(r, f, m, i);
            }
            Err(fastwarp_reg::RegError::Cancelled)
        }
    }

    #[test]
    fn a_cancel_inside_a_native_coreg_call_stops_the_run_without_an_ants_fallback() {
        for masked_only in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "dbsloc_coreg_cancel_in_call_{masked_only}_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let fixed = root.join("anchor.nii.gz");
            let moving = root.join("ct.nii.gz");
            reg_blob(&fixed, 0.0);
            reg_blob(&moving, 3.0);
            let dir = root.join("coregistration");
            std::fs::create_dir_all(dir.join("log")).unwrap();
            let paths = CtPaths {
                warped: dir.join("ct_in_anchor.nii.gz"),
                record: dir.join("log").join("ct_multistart.json"),
                work_dir: dir.join("log").join("multistart"),
            };
            let staged = Staged::new(vec![paths.warped.clone()]).unwrap();
            let tmp_prefix = crate::outputs::partial_of(&dir.join("ct_in_anchor"));
            let reg = RegChoice {
                requested: RegBackend::Auto,
                resolution: Resolution::Wgpu,
                engine: None,
            };
            let r = coregister_native(
                &CancelInCall { masked_only },
                &reg,
                &moving,
                &fixed,
                &paths,
                &staged,
                &tmp_prefix,
                None,
                None,
                None,
            );
            assert!(
                matches!(r, Err(PipelineError::Cancelled)),
                "{masked_only}: {r:?}"
            );
            assert!(!paths.warped.exists());
            let _ = std::fs::remove_dir_all(&root);
        }

        let mut case = norm_case("coreg_mr_cancel", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let fixed = sub.anchor_in_use().unwrap();
        let mut reg = RegChoice {
            requested: RegBackend::Auto,
            resolution: Resolution::Wgpu,
            engine: Some(Box::new(CancelInCall { masked_only: false })),
        };
        let mut calls = Vec::new();
        let e = {
            let mut exec = fake_rigid_ants(&mut calls, moving.clone(), [0.0; 3]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &fixed,
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
            .unwrap_err()
        };
        assert!(matches!(e, PipelineError::Cancelled), "{e}");
        assert!(calls.is_empty());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    fn native_reg() -> RegChoice {
        RegChoice {
            requested: RegBackend::Auto,
            resolution: Resolution::Wgpu,
            engine: Some(Box::new(crate::reg_backend::CpuReferenceEngine)),
        }
    }

    fn fake_ants_path() -> PipelineResult<PathBuf> {
        Ok(PathBuf::from("antsRegistration"))
    }

    fn add_postop_mr(case: &mut NormCase, suffix: &str, write: fn(&Path)) {
        write(&case.root.join(format!(
            "rawdata/sub-1/ses-postop/anat/sub-1_ses-postop_{suffix}.nii.gz"
        )));
        case.sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
    }

    fn fake_rigid_ants(
        calls: &mut Vec<Vec<String>>,
        image: PathBuf,
        t: [f64; 3],
    ) -> impl FnMut(&[String]) -> PipelineResult<()> + '_ {
        move |argv: &[String]| {
            calls.push(argv.to_vec());
            let out = arg_after(argv, "--output");
            let inner = out.trim_start_matches('[').trim_end_matches(']');
            let (pfx, warped) = match inner.split_once(',') {
                Some((p, w)) => (p, Some(w)),
                None => (inner, None),
            };
            write_linear_output(Path::new(pfx), &AffineTransform::from_translation(t)).unwrap();
            if let Some(w) = warped {
                std::fs::copy(&image, w).unwrap();
            }
            Ok(())
        }
    }

    fn precoreg_prefix(sub: &Subject, _template: &Path) -> PathBuf {
        derivatives::work_prefix(&derivatives::coreg_transform_dir(sub), "pre")
    }

    fn rms_over_range(a: &Volume<f64>, b: &Volume<f64>) -> f64 {
        let n = a.data().len() as f64;
        let ss: f64 = a
            .data()
            .iter()
            .zip(b.data())
            .map(|(x, y)| (x - y).powi(2))
            .sum();
        let lo = b.data().iter().copied().fold(f64::INFINITY, f64::min);
        let hi = b.data().iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (ss / n).sqrt() / (hi - lo)
    }

    #[test]
    fn precoreg_native_writes_the_reheadered_anchor_the_matrix_and_the_record() {
        let case = norm_case_with("precoreg_native", false, false, blob);
        let sub = &case.sub;
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let never_ants = || -> PipelineResult<PathBuf> { panic!("no ANTs on the wgpu backend") };
        let mut exec = never_exec;
        let out = crate::precoreg::precoreg_with(
            sub,
            &case.opts,
            native_reg(),
            &never_ants,
            &mut exec,
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            StageOutcome::done("ok:precoreg:sub-1_ses-preop_desc-precoreg_T1w")
        );
        let image = derivatives::precoreg_image(sub);
        let matrix = derivatives::precoreg_transform(sub, &space);
        let record = derivatives::precoreg_record(sub);
        for f in [&image, &matrix, &record] {
            assert!(f.is_file(), "missing {}", f.display());
            assert!(no_partials(f.parent().unwrap()));
        }
        let rec: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(rec["backend"], json!("wgpu"));
        assert_eq!(rec["template_space"], json!(space));
        assert_eq!(rec["reg_requested"], json!("auto"));
        assert_eq!(rec["reg_adapter"]["name"], json!("CPU reference"));
        assert_eq!(rec["matrix_ras"].as_array().unwrap().len(), 4);
        assert_eq!(rec["rotation_deg"].as_array().unwrap().len(), 3);
        assert_eq!(rec["translation_mm"].as_array().unwrap().len(), 3);

        let raw = sub.preop_anchor_raw.as_deref().unwrap();
        let moving = Volume::<f32>::load_nifti(raw).unwrap();
        let pre = Volume::<f32>::load_nifti(&image).unwrap();
        assert_eq!(moving.data(), pre.data());

        let chain = read_transform_file(&matrix).unwrap();
        let tpl_grid = fastwarp_reg::nifti_io::read_grid(&case.t1_tpl).unwrap();
        let want = fastwarp_reg::apply::apply_chain(
            &moving.to_f64(),
            &tpl_grid,
            &chain,
            Interpolation::Linear,
            0.0,
        )
        .unwrap();
        let got = fastwarp_reg::apply::apply_chain(
            &pre.to_f64(),
            &tpl_grid,
            &TransformChain::new(),
            Interpolation::Linear,
            0.0,
        )
        .unwrap();
        let rms = rms_over_range(&got, &want);
        assert!(rms < 0.01, "RMS {rms} of the robust range");

        assert_eq!(sub.anchor_in_use(), Some(image.clone()));
        assert_eq!(sub.anchor_best(), Some(raw));
        let inputs = crate::locks::default_lock_input_paths(sub);
        assert_eq!(inputs["precoreg"], vec![raw.to_path_buf()]);
        assert_eq!(inputs["coreg"][0], image);
        assert_eq!(inputs["normalize"][0], image);

        let again = crate::precoreg::precoreg_with(
            sub,
            &case.opts,
            native_reg(),
            &never_ants,
            &mut exec,
            None,
        )
        .unwrap();
        assert_eq!(again, out);
        let rec2: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
        assert_eq!(rec2["matrix_ras"], rec["matrix_ras"]);
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn precoreg_on_ants_runs_the_snapshot_argv_and_records_the_inverse_map() {
        let case = norm_case("precoreg_ants", false, false);
        let sub = &case.sub;
        let image = sub.anchor_best().unwrap().to_path_buf();
        let mut calls = Vec::new();
        let out = {
            let mut exec = fake_rigid_ants(&mut calls, image.clone(), [3.0, -2.0, 5.0]);
            crate::precoreg::precoreg_with(
                sub,
                &case.opts,
                RegChoice::ants(RegBackend::Ants),
                &fake_ants_path,
                &mut exec,
                None,
            )
        }
        .unwrap();
        assert_eq!(
            out,
            StageOutcome::done("ok:precoreg:sub-1_ses-preop_desc-precoreg_T1w")
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            crate::ants_argv::build_ants_precoreg_argv(
                Path::new("antsRegistration"),
                &image,
                &case.t1_tpl,
                &precoreg_prefix(sub, &case.t1_tpl),
                &[],
            )
        );
        let rec: Value = serde_json::from_str(
            &std::fs::read_to_string(derivatives::precoreg_record(sub)).unwrap(),
        )
        .unwrap();
        assert_eq!(rec["backend"], json!("ants"));
        assert_eq!(rec["translation_mm"], json!([3.0, -2.0, -5.0]));
        assert!(rec.get("reg_adapter").is_none());
        let a = fastwarp_reg::nifti_io::read_grid(&derivatives::precoreg_image(sub))
            .unwrap()
            .ras_affine();
        for (r, want) in [3.0, -2.0, -5.0].into_iter().enumerate() {
            assert!((a[r][3] - want).abs() < 1e-5, "{a:?}");
            assert!((a[r][r] - 1.0).abs() < 1e-6, "{a:?}");
        }
        assert!(no_partials(&sub.coreg_dir));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn precoreg_without_a_template_fails_and_writes_nothing() {
        let mut case = norm_case_with("precoreg_notpl", false, false, blob);
        case.opts.normalization_template = None;
        let sub = &case.sub;
        let mut exec = never_exec;
        let e = crate::precoreg::precoreg_with(
            sub,
            &case.opts,
            RegChoice::ants(RegBackend::Ants),
            &fake_ants_path,
            &mut exec,
            None,
        )
        .unwrap_err();
        assert!(
            e.to_string()
                .starts_with("precoreg: no normalization template"),
            "{e}"
        );
        assert!(!derivatives::precoreg_image(sub).exists());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn registration_stages_read_the_precoreg_image_once_it_exists() {
        let mut case = norm_case("anchor_switch", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        let raw = sub.preop_anchor_raw.clone().unwrap();
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(
            calls[0],
            build_ants_syn_argv(
                Path::new("antsRegistration"),
                &raw,
                &case.t1_tpl,
                &norm_work_prefix(sub),
                &[],
            )
        );
        std::fs::copy(&raw, derivatives::precoreg_image(sub)).unwrap();
        let precoreg = derivatives::precoreg_image(sub);
        let mut calls = Vec::new();
        {
            let mut exec = fake_ants(&mut calls, precoreg.clone(), false);
            normalize_with(sub, &case.opts, &tools(), &mut exec, None).unwrap();
        }
        assert_eq!(
            calls[0],
            build_ants_syn_argv(
                Path::new("antsRegistration"),
                &precoreg,
                &case.t1_tpl,
                &norm_work_prefix(sub),
                &[],
            )
        );

        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let mut calls = Vec::new();
        let mut reg = RegChoice::ants(RegBackend::Ants);
        let name = {
            let mut exec = fake_rigid_ants(&mut calls, moving.clone(), [0.5, 0.0, 0.0]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &sub.anchor_in_use().unwrap(),
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
        }
        .unwrap();
        assert_eq!(
            name,
            "sub-1_ses-postop_acq-ax_space-anchorNative_desc-preproc_T1w"
        );
        assert_eq!(
            calls[0],
            build_ants_mr_coreg_argv(
                Path::new("antsRegistration"),
                &moving,
                &precoreg,
                &coreg_work_prefix(sub, "ax_MRI"),
                &[],
            )
        );

        let mut fresh = sub.clone();
        fresh.refresh_bundles();
        let pairs = crate::checkreg::coreg_slide_specs(&fresh, &sub.log_dir.join("method.json"));
        let mr = pairs
            .iter()
            .find(|p| p.modality == "ax_MRI")
            .expect("a checkreg pair for the post-op MR");
        assert_eq!(mr.fixed, absolute(&precoreg));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn coreg_dispatch_runs_the_ct_then_every_mr_key_and_names_the_first_result() {
        use std::cell::RefCell;
        let mut case = norm_case("coreg_order", false, true);
        add_postop_mr(&mut case, "acq-sag_T1w", nii);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        add_postop_mr(&mut case, "acq-cor_T1w", nii);
        let sub = &case.sub;
        let seen: RefCell<Vec<(String, PathBuf)>> = RefCell::new(Vec::new());
        let run = |sub: &Subject| {
            seen.borrow_mut().clear();
            coregister_with(
                sub,
                RegChoice::ants(RegBackend::Ants),
                &mut |_, moving, fixed, _| {
                    seen.borrow_mut().push(("CT".into(), fixed.to_path_buf()));
                    assert!(moving.ends_with("sub-1_ses-postop_CT.nii.gz"));
                    Ok("ct-token".into())
                },
                &mut |_, key, _, fixed, _| {
                    seen.borrow_mut().push((key.into(), fixed.to_path_buf()));
                    Ok(format!("mr-{key}"))
                },
            )
        };
        let out = run(sub).unwrap();
        assert_eq!(out, StageOutcome::done("ok:coreg:ct-token"));
        let order: Vec<String> = seen.borrow().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(order, ["CT", "ax_MRI", "cor_MRI", "sag_MRI"]);
        let anchor = sub.anchor_in_use().unwrap();
        assert!(seen.borrow().iter().all(|(_, fixed)| *fixed == anchor));

        std::fs::remove_file(sub.postop_ct_raw.as_deref().unwrap()).unwrap();
        let sub_mr = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let out = run(&sub_mr).unwrap();
        assert_eq!(out, StageOutcome::done("ok:coreg:mr-ax_MRI"));
        let order: Vec<String> = seen.borrow().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(order, ["ax_MRI", "cor_MRI", "sag_MRI"]);
        let _ = std::fs::remove_dir_all(&case.root);
    }

    fn staged_argv_names(calls: &[Vec<String>]) -> Vec<String> {
        let mut names = Vec::new();
        for argv in calls {
            for arg in argv {
                for part in arg.split([',', '[', ']']) {
                    if part.contains(crate::outputs::PARTIAL_PREFIX) {
                        names.push(file_name_of(Path::new(part)));
                    }
                }
            }
        }
        names
    }

    #[test]
    fn every_staged_path_handed_to_ants_has_a_short_file_name() {
        let mut case = norm_case("short_names", true, true);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        nii(&derivatives::coreg_mr_image(&case.sub, "ax_MRI"));
        case.sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let (res, mut calls) = run_fake(&case, false);
        res.unwrap();
        let sub = &case.sub;
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let mut reg = RegChoice::ants(RegBackend::Ants);
        {
            let mut exec = fake_rigid_ants(&mut calls, moving.clone(), [0.5, 0.0, 0.0]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &sub.anchor_in_use().unwrap(),
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
            .unwrap();
        }
        let n4 = crate::ants_argv::build_n4_argv(
            Path::new("N4BiasFieldCorrection"),
            &moving,
            &Staged::mapped(vec![(
                derivatives::preproc_image(sub),
                derivatives::short_image(&sub.preproc_dir, "n4"),
            )])
            .unwrap()
            .tmp(&derivatives::preproc_image(sub)),
        );
        calls.push(n4);
        let mask_files = crate::ct_mask::tests::files("short_names_mask", [0.0; 3], [0.0; 3]);
        calls.extend(crate::ct_mask::tests::recorded_argv(&mask_files));
        let _ = std::fs::remove_dir_all(&mask_files.dir);
        let names = staged_argv_names(&calls);
        assert!(names.len() >= 10, "{names:?}");
        for n in &names {
            assert!(n.len() <= 32, "{n} is {} characters", n.len());
        }
        assert!(
            calls.iter().flatten().all(|a| !a.contains("desc-preproc")
                || !a.contains(crate::outputs::PARTIAL_PREFIX)),
            "{calls:#?}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn coreg_writes_a_method_log_whose_approval_a_rerun_resets() {
        let mut case = norm_case("coreg_log", false, true);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        let run = |sub: &Subject| {
            coregister_with(
                sub,
                RegChoice::ants(RegBackend::Ants),
                &mut |_, _, _, _| Ok("ct".into()),
                &mut |_, key, _, _, _| Ok(format!("mr-{key}")),
            )
            .unwrap()
        };
        run(sub);
        let log_path = derivatives::coreg_method_log(sub);
        let read = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&log_path).unwrap()).unwrap()
        };
        let log = read();
        assert_eq!(log["method"], json!(COREG_METHOD_CT));
        assert_eq!(log["engine"], json!("ants"));
        assert_eq!(log["custom"], json!(false));
        assert_eq!(log["version"], json!(env!("CARGO_PKG_VERSION")));
        assert_eq!(log["approval"], json!({"CT": 0, "ax_MRI": 0}));
        assert_eq!(
            log["CT"]["transform"],
            json!("sub-1_ses-postop_from-CT_to-anchorNative_mode-image_xfm.mat")
        );
        assert_eq!(
            log["ax_MRI"]["transform"],
            json!("sub-1_ses-postop_acq-ax_from-T1w_to-anchorNative_mode-image_xfm.mat")
        );
        assert!(!crate::checkreg::method_log_allows_continue(&log_path));
        crate::checkreg::apply_approval(&log_path, "coreg", Some("CT"), true).unwrap();
        crate::checkreg::apply_approval(&log_path, "coreg", Some("ax_MRI"), true).unwrap();
        assert!(crate::checkreg::method_log_allows_continue(&log_path));
        run(sub);
        assert_eq!(read()["approval"], json!({"CT": 0, "ax_MRI": 0}));
        assert!(!crate::checkreg::method_log_allows_continue(&log_path));
        assert!(no_partials(&derivatives::coreg_log_dir(sub)));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn a_failing_mr_job_keeps_every_previous_coreg_output_and_the_method_log() {
        let mut case = norm_case("coreg_atomic", false, true);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        let ct_image = derivatives::coreg_ct_image(sub);
        let log_path = derivatives::coreg_method_log(sub);
        for p in [&ct_image, &log_path] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"previous").unwrap();
        }
        let e = coregister_with(
            sub,
            RegChoice::ants(RegBackend::Ants),
            &mut |_, _, _, staged| {
                std::fs::write(staged.tmp(&derivatives::coreg_ct_image(sub)), b"new").unwrap();
                Ok("ct".into())
            },
            &mut |_, _, _, _, _| Err(PipelineError::Message("mr failed".into())),
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "mr failed");
        assert_eq!(std::fs::read(&ct_image).unwrap(), b"previous");
        assert_eq!(std::fs::read(&log_path).unwrap(), b"previous");
        assert!(no_partials(&sub.coreg_dir));
        assert!(no_partials(&derivatives::coreg_log_dir(sub)));
        assert!(no_partials(&derivatives::coreg_transform_dir(sub)));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn the_coreg_backend_rule_reads_every_image_of_the_method_log() {
        let (root, sub) = scratch("coreg_lock_log");
        let with = |b: RegBackend| RunOptions {
            reg_backend: b,
            ..RunOptions::new(&root, "1")
        };
        let log_path = derivatives::coreg_method_log(&sub);
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        let entry = |backend: &str| json!({"method": COREG_METHOD_MR, "backend": backend, "image": "i.nii.gz", "transform": "t.mat"});
        let write = |log: Value| std::fs::write(&log_path, log.to_string()).unwrap();
        write(json!({"engine": "wgpu", "approval": {"ax_MRI": 0}, "ax_MRI": entry("wgpu")}));
        coreg_approved_outputs_match(&sub, &with(RegBackend::Wgpu)).unwrap();
        coreg_approved_outputs_match(&sub, &with(RegBackend::Auto)).unwrap();
        let e = coreg_approved_outputs_match(&sub, &with(RegBackend::Ants)).unwrap_err();
        assert!(e.contains("made with the wgpu registration backend"), "{e}");
        write(json!({"engine": "wgpu", "CT": entry("ants"), "ax_MRI": entry("wgpu")}));
        for asked in [RegBackend::Ants, RegBackend::Wgpu] {
            assert!(coreg_approved_outputs_match(&sub, &with(asked)).is_err());
        }
        write(json!({"engine": "wgpu", "ax_MRI": {"image": "i.nii.gz"}}));
        coreg_approved_outputs_match(&sub, &with(RegBackend::Wgpu)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_precoreg_lock_follows_the_registration_backend_rule() {
        let (root, sub) = scratch("precoreg_lock");
        let with = |b: RegBackend| RunOptions {
            reg_backend: b,
            ..RunOptions::new(&root, "1")
        };
        crate::precoreg::precoreg_approved_outputs_match(&sub, &with(RegBackend::Wgpu)).unwrap();
        let record = derivatives::precoreg_record(&sub);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        for recorded in [None, Some("ants"), Some("wgpu")] {
            let mut rec = json!({"backend": recorded.unwrap_or("ants")});
            if let Some(b) = recorded {
                rec["reg_backend"] = json!(b);
            }
            std::fs::write(&record, rec.to_string()).unwrap();
            let have = recorded.unwrap_or("ants");
            crate::precoreg::precoreg_approved_outputs_match(&sub, &with(RegBackend::Auto))
                .unwrap();
            for asked in [RegBackend::Ants, RegBackend::Wgpu] {
                let r = crate::precoreg::precoreg_approved_outputs_match(&sub, &with(asked));
                assert_eq!(r.is_ok(), asked.as_str() == have, "{recorded:?} {asked:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn coreg_without_any_post_op_image_fails_before_touching_a_backend() {
        let (root, sub) = scratch("coreg_none");
        let e = run_coregister(&sub, &RunOptions::new(&root, "1"), None, None).unwrap_err();
        assert_eq!(e.to_string(), "no postop CT or MR");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mr_coregistration_on_ants_stages_the_transform_and_the_warped_image() {
        let mut case = norm_case("coreg_mr_ants", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let fixed = sub.anchor_in_use().unwrap();
        let stale = derivatives::coreg_mr_image(sub, "ax_MRI");
        std::fs::write(&stale, b"stale").unwrap();
        let mut calls = Vec::new();
        let mut reg = RegChoice::ants(RegBackend::Ants);
        {
            let mut exec = fake_rigid_ants(&mut calls, moving.clone(), [1.0, 2.0, 3.0]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &fixed,
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            build_ants_mr_coreg_argv(
                Path::new("antsRegistration"),
                &moving,
                &fixed,
                &coreg_work_prefix(sub, "ax_MRI"),
                &[],
            )
        );
        assert!(derivatives::coreg_mr_affine(sub, "ax_MRI").is_file());
        assert!(Volume::<f32>::load_nifti(&stale).is_ok());
        assert!(no_partials(&sub.coreg_dir));
        assert!(no_partials(&derivatives::coreg_transform_dir(sub)));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn an_empty_warped_mr_fails_the_stage_and_keeps_the_previous_outputs() {
        let mut case = norm_case("coreg_mr_empty", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let fixed = sub.anchor_in_use().unwrap();
        let warped = derivatives::coreg_mr_image(sub, "ax_MRI");
        std::fs::write(&warped, b"previous").unwrap();
        let blank = case.root.join("blank.nii.gz");
        let zeros = ndarray::Array3::<u8>::zeros((6, 6, 6));
        crate::nifti_io::write_u8_3d(&blank, &zeros, &IDENT).unwrap();
        let mut calls = Vec::new();
        let mut reg = RegChoice::ants(RegBackend::Ants);
        let e = {
            let mut exec = fake_rigid_ants(&mut calls, blank, [0.0; 3]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &fixed,
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
        }
        .unwrap_err();
        assert!(
            e.to_string().starts_with("coreg: warped ax_MRI is empty"),
            "{e}"
        );
        assert_eq!(std::fs::read(&warped).unwrap(), b"previous");
        assert!(no_partials(&sub.coreg_dir));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn mr_coregistration_on_the_in_process_engine_recovers_a_shift() {
        let mut case = norm_case_with("coreg_mr_native", false, false, |p| reg_blob(p, 0.0));
        add_postop_mr(&mut case, "acq-ax_T1w", |p| reg_blob(p, 3.0));
        let sub = &case.sub;
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let fixed = sub.anchor_in_use().unwrap();
        let mut reg = native_reg();
        let mut exec = never_exec;
        let never_ants = || -> PipelineResult<PathBuf> { panic!("no ANTs on the wgpu backend") };
        let name = coregister_mr_with(
            sub,
            &case.opts,
            &mut reg,
            "ax_MRI",
            &moving,
            &fixed,
            &never_ants,
            &mut exec,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            name,
            "sub-1_ses-postop_acq-ax_space-anchorNative_desc-preproc_T1w"
        );
        let warped = derivatives::coreg_mr_image(sub, "ax_MRI");
        assert!(!warped_is_empty(&warped).unwrap());
        let grid = fastwarp_reg::nifti_io::read_grid(&warped).unwrap();
        assert!(grid.approx_eq(&fastwarp_reg::nifti_io::read_grid(&fixed).unwrap(), 1e-6));
        let chain = read_transform_file(&derivatives::coreg_mr_affine(sub, "ax_MRI")).unwrap();
        let moved = chain.apply([0.0; 3]);
        let shift = (moved[0].powi(2) + moved[1].powi(2) + moved[2].powi(2)).sqrt();
        assert!((2.0..4.0).contains(&shift), "recovered shift {shift} mm");
        assert!(no_partials(&sub.coreg_dir));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn a_failing_in_process_mr_coregistration_falls_back_to_ants_under_auto_only() {
        let mut case = norm_case("coreg_mr_fallback", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let sub = &case.sub;
        std::fs::create_dir_all(&sub.coreg_dir).unwrap();
        let moving = sub.postop_mr_images()[0].1.to_path_buf();
        let fixed = sub.anchor_in_use().unwrap();
        let failing = |requested| RegChoice {
            requested,
            resolution: Resolution::Wgpu,
            engine: Some(Box::new(RigidFailingEngine)),
        };
        let mut calls = Vec::new();
        let mut reg = failing(RegBackend::Auto);
        {
            let mut exec = fake_rigid_ants(&mut calls, moving.clone(), [0.0; 3]);
            coregister_mr_with(
                sub,
                &case.opts,
                &mut reg,
                "ax_MRI",
                &moving,
                &fixed,
                &fake_ants_path,
                &mut exec,
                None,
                None,
            )
            .unwrap();
        }
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            reg.resolution,
            Resolution::Ants { reason: Some(_) }
        ));
        let mut reg = failing(RegBackend::Wgpu);
        let mut exec = never_exec;
        let e = coregister_mr_with(
            sub,
            &case.opts,
            &mut reg,
            "ax_MRI",
            &moving,
            &fixed,
            &fake_ants_path,
            &mut exec,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("--reg-backend ants"), "{e}");
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn normalize_on_ants_applies_the_composite_to_the_ct_and_every_coregistered_mr() {
        let mut case = norm_case("norm_mr", false, true);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        add_postop_mr(&mut case, "acq-cor_T1w", nii);
        nii(&derivatives::coreg_mr_image(&case.sub, "ax_MRI"));
        case.sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let sub = &case.sub;
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        std::fs::create_dir_all(&sub.norm_dir).unwrap();
        let stale = derivatives::norm_postop_image(sub, "cor_MRI", &space);
        std::fs::write(&stale, b"stale").unwrap();
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 3, "{calls:#?}");
        let partial_composite = suffixed(&norm_work_prefix(sub), ANTS_COMPOSITE_SUFFIX);
        let ct_out = derivatives::norm_postop_image(sub, "CT", &space);
        let ax_out = derivatives::norm_postop_image(sub, "ax_MRI", &space);
        assert_eq!(
            calls[1],
            build_ants_apply_transforms_argv(
                Path::new("antsApplyTransforms"),
                &derivatives::coreg_ct_image(sub),
                &case.t1_tpl,
                &postop_work_image(sub, "CT"),
                &partial_composite,
                &[],
            )
        );
        assert_eq!(
            calls[2],
            build_ants_apply_transforms_argv(
                Path::new("antsApplyTransforms"),
                &derivatives::coreg_mr_image(sub, "ax_MRI"),
                &case.t1_tpl,
                &postop_work_image(sub, "ax_MRI"),
                &partial_composite,
                &[],
            )
        );
        assert!(ct_out.is_file());
        assert!(ax_out.is_file());
        assert!(!stale.exists());
        assert!(no_partials(&sub.norm_dir));
        let fresh = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        assert_eq!(fresh.norm_postop_mr_in_use(), Some(ax_out));
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn normalize_for_an_mr_only_subject_writes_the_mr_output_once_it_is_coregistered() {
        let mut case = norm_case("norm_mr_only", false, false);
        add_postop_mr(&mut case, "acq-ax_T1w", nii);
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 1, "no post-op image is coregistered yet");
        let out = derivatives::norm_postop_image(&case.sub, "ax_MRI", &space);
        assert!(!out.exists());
        nii(&derivatives::coreg_mr_image(&case.sub, "ax_MRI"));
        case.sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let (res, calls) = run_fake(&case, false);
        res.unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1][0], "antsApplyTransforms");
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn native_normalize_applies_the_composite_to_the_coregistered_mr() {
        let mut case = norm_case_with("native_mr", false, false, blob);
        add_postop_mr(&mut case, "acq-ax_T1w", blob);
        blob(&derivatives::coreg_mr_image(&case.sub, "ax_MRI"));
        case.sub = crate::bids::resolve_subject_with(&case.root, "1", false).unwrap();
        let sub = &case.sub;
        let space = crate::template_catalog::infer_space(&case.t1_tpl);
        let tools = native_tools(Box::new(crate::reg_backend::CpuReferenceEngine));
        let mut exec = never_exec;
        normalize_with(sub, &case.opts, &tools, &mut exec, None).unwrap();
        let out = derivatives::norm_postop_image(sub, "ax_MRI", &space);
        assert!(out.is_file());
        assert!(!derivatives::norm_postop_image(sub, "CT", &space).exists());
        let pair = fastwarp_reg::transform::CompositePair {
            forward: read_transform_file(&derivatives::norm_forward(sub, &space)).unwrap(),
            inverse: read_transform_file(&derivatives::norm_inverse(sub, &space)).unwrap(),
        };
        let tpl = fastwarp_reg::nifti_io::read_grid(&case.t1_tpl).unwrap();
        let got = Volume::<f32>::load_nifti(&out).unwrap();
        assert!(got.grid().approx_eq(&tpl, 1e-6));
        let src = Volume::<f32>::load_nifti(&derivatives::coreg_mr_image(sub, "ax_MRI"))
            .unwrap()
            .to_f64();
        let want =
            fastwarp_reg::apply::apply_chain(&src, &tpl, &pair.forward, Interpolation::Linear, 0.0)
                .unwrap();
        for (a, b) in got.data().iter().zip(want.data()) {
            assert!((f64::from(*a) - b).abs() < 1e-3);
        }
        let rec = read_record(&case);
        assert!(
            rec["reg_timings"]["ax_MRI_apply_s"].as_f64().is_some(),
            "{rec}"
        );
        let _ = std::fs::remove_dir_all(&case.root);
    }

    #[test]
    fn reconstruct_on_post_op_mri_reads_ax_then_cor_then_sag() {
        let (root, _) = scratch("recon_mr_order");
        let post = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        for acq in ["sag", "cor", "ax"] {
            std::fs::write(
                post.join(format!("sub-1_ses-postop_acq-{acq}_T1w.nii.gz")),
                b"x",
            )
            .unwrap();
        }
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let space = "MNI152NLin2009bAsym";
        std::fs::create_dir_all(derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            derivatives::norm_method_log(&sub),
            format!(r#"{{"template_space": "{space}"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(&sub.norm_dir).unwrap();
        for key in ["sag_MRI", "cor_MRI", "ax_MRI"] {
            std::fs::write(derivatives::norm_postop_image(&sub, key, space), b"x").unwrap();
        }
        let opts = RunOptions {
            el_model: Some("Medtronic 3387".into()),
            ..RunOptions::new(&root, "1")
        };
        let attempt = || {
            let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
            reconstruct_template(&sub, &opts, None, None, &[])
                .unwrap_err()
                .to_string()
        };
        let msg = attempt();
        assert!(msg.contains("acq-ax"), "{msg}");
        std::fs::remove_file(derivatives::norm_postop_image(&sub, "ax_MRI", space)).unwrap();
        let msg = attempt();
        assert!(msg.contains("acq-cor") && !msg.contains("acq-sag"), "{msg}");
        std::fs::remove_file(derivatives::norm_postop_image(&sub, "cor_MRI", space)).unwrap();
        let msg = attempt();
        assert!(msg.contains("acq-sag"), "{msg}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failing_template_trace_next_to_a_kept_side_is_left_out() {
        let (root, _) = scratch("recon_mr_kept");
        let post = root.join("rawdata/sub-1/ses-postop/anat");
        std::fs::create_dir_all(&post).unwrap();
        std::fs::write(post.join("sub-1_ses-postop_acq-ax_T1w.nii.gz"), b"x").unwrap();
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let space = "MNI152NLin2009bAsym";
        std::fs::create_dir_all(derivatives::norm_log_dir(&sub)).unwrap();
        std::fs::write(
            derivatives::norm_method_log(&sub),
            format!(r#"{{"template_space": "{space}"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(&sub.norm_dir).unwrap();
        let aff: Mat4 = [
            [1.0, 0.0, 0.0, -40.0],
            [0.0, 1.0, 0.0, -40.0],
            [0.0, 0.0, 1.0, -40.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        crate::nifti_io::write_u8_3d(
            &derivatives::norm_postop_image(&sub, "ax_MRI", space),
            &ndarray::Array3::<u8>::zeros((80, 80, 80)),
            &aff,
        )
        .unwrap();
        let sub = crate::bids::resolve_subject_with(&root, "1", false).unwrap();
        let opts = RunOptions {
            el_model: Some("Medtronic 3387".into()),
            ..RunOptions::new(&root, "1")
        };
        let err = reconstruct_template(&sub, &opts, None, None, &[]).unwrap_err();
        assert!(err.to_string().contains("E_RECON_trace"), "{err}");
        let mut reco = reconstruct_template(&sub, &opts, None, None, &[SIDE_RIGHT]).unwrap();
        assert!(reco["mni"].as_object().unwrap().is_empty());
        finish_reconstruction(&sub, &opts, &mut reco, None, EditSpace::Mni, None, None).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(sub.reconstruction_json()).unwrap())
                .unwrap();
        let meta = &written["meta"];
        assert_eq!(meta["software"]["version"], crate::version::VERSION);
        assert_eq!(meta["software"]["git_hash"], crate::version::GIT_HASH);
        assert_eq!(meta["run_options"]["subject"], "1");
        assert_eq!(meta["settings"]["recon_method"], "trac_core");
        assert_eq!(meta["settings"]["recon_bright_metal"], false);
        assert_eq!(meta["settings"]["reg_backend"]["normalize"], "ants");
        assert_eq!(meta["settings"]["norm_multispectral"], Value::Null);
        let rec = &reco["meta"]["reconstruct"];
        assert_eq!(rec["status"][SIDE_LEFT], "failed");
        assert!(rec["status"].get(SIDE_RIGHT).is_none());
        assert!(rec["failed"][SIDE_LEFT]
            .as_str()
            .unwrap()
            .contains("E_RECON_trace"));
        let _ = std::fs::remove_dir_all(&root);
    }

    struct RigidFailingEngine;

    impl crate::reg_backend::RegEngine for RigidFailingEngine {
        fn describe(&self) -> Value {
            json!({"name": "rigid failing"})
        }

        fn label(&self) -> String {
            "Rigid failing test engine (test, Cpu)".to_owned()
        }

        fn register(
            &self,
            _: &fastwarp_reg::register::LinearPlan,
            _: &fastwarp_reg::recipe::SynRecipe,
            _: &[fastwarp_reg::Volume<f64>],
            _: &[fastwarp_reg::Volume<f64>],
            _: Option<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::register::RegisterOutput> {
            Err(fastwarp_reg::RegError::Invalid {
                what: "test",
                message: "unsupported".into(),
            })
        }

        fn rigid(
            &self,
            _: &fastwarp_reg::linear::LinearRecipe,
            _: &fastwarp_reg::Volume<f64>,
            _: &fastwarp_reg::Volume<f64>,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            Err(fastwarp_reg::RegError::Invalid {
                what: "test",
                message: "out of memory in the test engine".into(),
            })
        }

        fn rigid_from(
            &self,
            _: &fastwarp_reg::linear::LinearRecipe,
            _: &fastwarp_reg::Volume<f64>,
            _: &fastwarp_reg::Volume<f64>,
            _: fastwarp_reg::transform::AffineTransform,
        ) -> fastwarp_reg::Result<fastwarp_reg::linear::RigidRegistrationResult> {
            Err(fastwarp_reg::RegError::Invalid {
                what: "test",
                message: "out of memory in the test engine".into(),
            })
        }
    }
}
