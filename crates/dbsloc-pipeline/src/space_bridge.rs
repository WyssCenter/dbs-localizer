// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The template space bridge (`specs/04` §5.4a): one registration from the reference template
//! (`MNI152NLin2009bAsym`) to any other template, cached beside the per-user data, so that
//! assets bound to the reference space can be mapped in and coordinates mapped out.
//!
//! With fixed = target template and moving = reference template, `Composite.h5` maps target
//! points to reference points and resamples reference images onto the target grid.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use rayon::prelude::*;
use serde_json::{json, Map, Value};

use fastwarp_core::Mat4;
use fastwarp_reg::apply::{apply_nifti, Interpolation};
use fastwarp_reg::transform::{read_transform_file, TransformChain};

use crate::ants_argv::{build_ants_syn_argv, build_ants_syn_multispectral_argv};
use crate::error::{PipelineError, PipelineResult};
use crate::options::RunOptions;
use crate::paths::py_float;
use crate::point_warp::{AntsPointWarp, PointWarp, PointWarpError, RustPointWarp};
use crate::protocol::RunEvent;
use crate::reg_backend::{RegChoice, Resolution};
use crate::stage_impl::{
    ants_registration_inputs, command_from, finest_grid_of, log, require_binary, spawn_logged,
    suffixed,
};
use crate::template_catalog::{
    default_entry, entry_t1_path, infer_space, user_data_dir, REFERENCE_SPACE,
};
use crate::templates::t2_sibling_candidates;

pub const BRIDGE_DISABLE_ENV: &str = "DBSLOC_NO_BRIDGE";
pub const BRIDGE_RECORD: &str = "bridge.json";
pub const AFFINE_LATTICE_STEPS: usize = 9;
pub const AFFINE_LATTICE_X_MM: [f64; 2] = [-70.0, 70.0];
pub const AFFINE_LATTICE_Y_MM: [f64; 2] = [-70.0, 70.0];
pub const AFFINE_LATTICE_Z_MM: [f64; 2] = [-50.0, 80.0];

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

const WARP_MEMORY_BUDGET_BYTES: usize = 2 << 30;
const WARP_BYTES_PER_VOXEL: usize = 24;
const WARP_MAX_PARALLEL_IMAGES: usize = 4;

const T1_LABEL: &str = "T1w";
const T2_LABEL: &str = "T2w";
const STAGE: &str = "bridge";

/// A built bridge: the cache directory, the target space id and the file prefix of the composite
/// pair (`<prefix>Composite.h5`, `<prefix>InverseComposite.h5`).
#[derive(Debug, Clone, PartialEq)]
pub struct Bridge {
    pub dir: PathBuf,
    pub target_space: String,
    pub prefix: PathBuf,
}

/// The least-squares affine of [`Bridge::affine_fit_to_reference`] with its RMS residual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffineFit {
    pub matrix: Mat4,
    pub rms_mm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheDecision {
    Use,
    Rebuild,
}

pub(crate) fn cache_decision(
    record: Option<&Value>,
    files_present: bool,
    reference_sha256: &str,
    target_sha256: &str,
) -> CacheDecision {
    let matches = |key: &str, want: &str| {
        record
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .is_some_and(|have| have == want)
    };
    if files_present
        && matches("reference_t1_sha256", reference_sha256)
        && matches("target_t1_sha256", target_sha256)
    {
        CacheDecision::Use
    } else {
        CacheDecision::Rebuild
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(128)
        .collect()
}

pub fn bridge_dir_name(target_space: &str, target_sha256: &str) -> String {
    let short: String = target_sha256.chars().take(12).collect();
    format!("{}__{short}", sanitize(target_space))
}

pub fn bridges_root(data_root: &Path) -> PathBuf {
    data_root.join("templates").join("bridges")
}

pub fn bridge_prefix(dir: &Path) -> PathBuf {
    dir.join(format!("from-{REFERENCE_SPACE}_"))
}

fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

pub(crate) fn now_iso() -> String {
    iso_utc(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    )
}

fn cancelled(cancel: Option<&AtomicBool>) -> PipelineResult<()> {
    if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        Err(PipelineError::Cancelled)
    } else {
        Ok(())
    }
}

fn io_err(what: &str, e: impl std::fmt::Display) -> PipelineError {
    PipelineError::Message(format!("{STAGE}: {what}: {e}"))
}

fn sha_of(p: &Path) -> PipelineResult<String> {
    crate::sha256::file_sha256_hex(p)
        .map_err(|e| io_err(&format!("cannot read {}", p.display()), e))
}

fn t2_beside(t1: &Path) -> Option<PathBuf> {
    t2_sibling_candidates(t1).into_iter().find(|p| p.is_file())
}

fn read_record(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(BRIDGE_RECORD)).ok()?).ok()
}

pub(crate) struct BridgeSources<'a> {
    pub data_root: &'a Path,
    pub reference_t1: &'a Path,
    pub target_t1: &'a Path,
    pub target_space: &'a str,
}

type Exec<'a> = dyn FnMut(&[String]) -> PipelineResult<()> + 'a;
type Choose<'a> = dyn FnMut() -> PipelineResult<RegChoice> + 'a;

pub fn ensure_bridge(
    target_t1: &Path,
    opts: &RunOptions,
    cancel: Option<&AtomicBool>,
    tx: Option<&Sender<RunEvent>>,
) -> PipelineResult<Option<Bridge>> {
    let target_space = infer_space(target_t1);
    if target_space == REFERENCE_SPACE {
        return Ok(None);
    }
    if std::env::var_os(BRIDGE_DISABLE_ENV).is_some_and(|v| !v.is_empty()) {
        log(
            tx,
            format!("dbsloc: note: bridge: disabled by {BRIDGE_DISABLE_ENV}"),
        );
        return Ok(None);
    }
    let data_root = user_data_dir().ok_or_else(|| {
        PipelineError::Message(format!(
            "{STAGE}: no per-user data folder (set {})",
            crate::template_catalog::DATA_DIR_ENV
        ))
    })?;
    let reference_t1 = entry_t1_path(default_entry()).ok_or_else(|| {
        PipelineError::Message(format!(
            "{STAGE}: the reference template {REFERENCE_SPACE} is not installed"
        ))
    })?;
    let src = BridgeSources {
        data_root: &data_root,
        reference_t1: &reference_t1,
        target_t1,
        target_space: &target_space,
    };
    let mut choose = || {
        crate::reg_backend::choose_for_run(opts.backend_for(crate::stages::Stage::Normalize))
            .map_err(|e| PipelineError::Message(format!("{STAGE}: {e}")))
    };
    let mut exec = |argv: &[String]| {
        let mut cmd = command_from(argv, opts);
        spawn_logged(&mut cmd, STAGE, cancel, tx)
    };
    ensure_bridge_with(&src, opts, &mut choose, &mut exec, cancel, tx).map(Some)
}

pub(crate) fn ensure_bridge_with(
    src: &BridgeSources<'_>,
    opts: &RunOptions,
    choose: &mut Choose<'_>,
    exec: &mut Exec<'_>,
    cancel: Option<&AtomicBool>,
    tx: Option<&Sender<RunEvent>>,
) -> PipelineResult<Bridge> {
    let target_sha = sha_of(src.target_t1)?;
    let dir = bridges_root(src.data_root).join(bridge_dir_name(src.target_space, &target_sha));
    let reference_sha = sha_of(src.reference_t1)?;
    let record = read_record(&dir);
    let present = {
        let prefix = bridge_prefix(&dir);
        suffixed(&prefix, "Composite.h5").is_file()
            && suffixed(&prefix, "InverseComposite.h5").is_file()
    };
    let bridge = Bridge {
        prefix: bridge_prefix(&dir),
        dir: dir.clone(),
        target_space: src.target_space.to_owned(),
    };
    if cache_decision(record.as_ref(), present, &reference_sha, &target_sha) == CacheDecision::Use {
        log(
            tx,
            format!("dbsloc: note: bridge: using cached {}", dir.display()),
        );
        return Ok(bridge);
    }
    log(
        tx,
        format!(
            "dbsloc: note: bridge: building {REFERENCE_SPACE} -> {} (once per template)",
            src.target_space
        ),
    );
    cancelled(cancel)?;
    let mut reg = choose()?;

    let reference_t2 = t2_beside(src.reference_t1);
    let target_t2 = t2_beside(src.target_t1);
    let t2_pair = reference_t2.zip(target_t2);
    let channels = if t2_pair.is_some() { 2 } else { 1 };
    let caps = reg.engine.as_deref().and_then(|e| e.capacity_caps());
    let grid = crate::norm_grid::resolve_for_template(
        opts.norm_isotropic_mm,
        Some(src.target_t1),
        channels,
        caps.as_ref(),
    );
    if let Some(note) = &grid.note {
        log(
            tx,
            format!(
                "dbsloc: warning: bridge: {note}; {}",
                crate::norm_grid::STEP_REMEDY
            ),
        );
    }
    let grid_mm = grid.mm;

    if reg.engine.is_some() {
        if let Some(dims) = finest_grid_of(src.target_t1, grid_mm) {
            match crate::reg_backend::capacity_gate(reg.requested, dims, channels, caps.as_ref()) {
                Ok(Some(note)) => {
                    log(tx, format!("dbsloc: note: bridge: {note}"));
                    reg = RegChoice {
                        requested: reg.requested,
                        resolution: Resolution::Ants { reason: Some(note) },
                        engine: None,
                    };
                }
                Ok(None) => {}
                Err(e) => return Err(PipelineError::Message(format!("{STAGE}: {e}"))),
            }
        }
    }

    let root = bridges_root(src.data_root);
    std::fs::create_dir_all(&root).map_err(|e| io_err("cannot create the cache folder", e))?;
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staging = root.join(format!(
        "{}{name}_{}_{}",
        crate::outputs::PARTIAL_PREFIX,
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| io_err("cannot create the staging folder", e))?;

    let built = build_in(
        src,
        opts,
        grid_mm,
        &reg,
        t2_pair.as_ref().map(|(r, t)| (r.as_path(), t.as_path())),
        &staging,
        exec,
        cancel,
        tx,
    );
    let extra = match built {
        Ok(extra) => extra,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    let finish = (|| -> PipelineResult<()> {
        let rec = json!({
            "reference_space": REFERENCE_SPACE,
            "reference_t1": src.reference_t1.to_string_lossy(),
            "reference_t1_sha256": reference_sha,
            "target_space": src.target_space,
            "target_t1": src.target_t1.to_string_lossy(),
            "target_t1_sha256": target_sha,
            "channels": if t2_pair.is_some() { json!([T1_LABEL, T2_LABEL]) } else { json!([T1_LABEL]) },
            "grid_mm": grid_mm,
            "grid_source": grid.source.as_str(),
            "backend": reg.resolution.as_str(),
            "created": now_iso(),
            "registration": Value::Object(extra),
        });
        std::fs::write(
            staging.join(BRIDGE_RECORD),
            serde_json::to_string_pretty(&rec)? + "\n",
        )?;
        let _ = std::fs::remove_dir_all(staging.join(resample_dir_name(grid_mm)));
        for f in ["Composite.h5", "InverseComposite.h5"] {
            let p = suffixed(&bridge_prefix(&staging), f);
            if !p.is_file() {
                return Err(PipelineError::Message(format!(
                    "{STAGE}: the registration wrote no {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                )));
            }
        }
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| io_err(&format!("cannot replace {}", dir.display()), e))?;
        }
        std::fs::rename(&staging, &dir)
            .map_err(|e| io_err(&format!("cannot create {}", dir.display()), e))
    })();
    if let Err(e) = finish {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    log(tx, format!("dbsloc: note: bridge: built {}", dir.display()));
    Ok(bridge)
}

fn resample_dir_name(iso_mm: Option<f64>) -> String {
    format!(
        "_resampled_is{}mm",
        iso_mm.map_or_else(String::new, py_float)
    )
}

#[allow(clippy::too_many_arguments)]
fn build_in(
    src: &BridgeSources<'_>,
    opts: &RunOptions,
    grid_mm: Option<f64>,
    reg: &RegChoice,
    t2_pair: Option<(&Path, &Path)>,
    staging: &Path,
    exec: &mut Exec<'_>,
    cancel: Option<&AtomicBool>,
    tx: Option<&Sender<RunEvent>>,
) -> PipelineResult<Map<String, Value>> {
    let prefix = bridge_prefix(staging);
    if let Some(engine) = reg.engine.as_deref() {
        let mut fixed = vec![src.target_t1];
        let mut moving = vec![src.reference_t1];
        if let Some((reference_t2, target_t2)) = t2_pair {
            fixed.push(target_t2);
            moving.push(reference_t2);
        }
        let mut extra = crate::norm_native::register_templates(
            &crate::norm_native::PairJob {
                engine,
                stage: STAGE,
                iso_mm: grid_mm,
                tmp_prefix: &prefix,
                cancel,
            },
            &fixed,
            &moving,
            opts.ants_max_threads,
            tx,
        )?;
        extra.insert("reg_adapter".into(), engine.describe());
        return Ok(extra);
    }
    let ants = require_binary("antsRegistration", STAGE)?;
    log(
        tx,
        format!(
            "[bridge] antsRegistration SyN {REFERENCE_SPACE} -> {} ({})",
            src.target_space,
            if t2_pair.is_some() {
                format!("{T1_LABEL} + {T2_LABEL}")
            } else {
                T1_LABEL.to_owned()
            }
        ),
    );
    let work = staging.join(resample_dir_name(grid_mm));
    let t2_moving = t2_pair.map(|(r, t)| (r.to_path_buf(), t.to_path_buf()));
    let (reg_moving, reg_fixed, reg_t2) = ants_registration_inputs(
        STAGE,
        &work,
        grid_mm,
        src.reference_t1,
        src.target_t1,
        t2_moving,
        tx,
    )?;
    cancelled(cancel)?;
    let argv = match &reg_t2 {
        Some((m2, f2)) => build_ants_syn_multispectral_argv(
            &ants,
            &reg_moving,
            &reg_fixed,
            m2,
            f2,
            &prefix,
            &opts.ants_extra,
        ),
        None => build_ants_syn_argv(&ants, &reg_moving, &reg_fixed, &prefix, &opts.ants_extra),
    };
    exec(&argv)?;
    let mut extra = Map::new();
    extra.insert("reg_tool".into(), json!("antsRegistration"));
    Ok(extra)
}

fn point_err(e: PointWarpError) -> String {
    e.to_string()
}

fn lattice_axis(range: [f64; 2]) -> Vec<f64> {
    let step = (range[1] - range[0]) / (AFFINE_LATTICE_STEPS - 1) as f64;
    (0..AFFINE_LATTICE_STEPS)
        .map(|i| range[0] + step * i as f64)
        .collect()
}

pub fn affine_lattice() -> Vec<[f64; 3]> {
    let (xs, ys, zs) = (
        lattice_axis(AFFINE_LATTICE_X_MM),
        lattice_axis(AFFINE_LATTICE_Y_MM),
        lattice_axis(AFFINE_LATTICE_Z_MM),
    );
    let mut out = Vec::with_capacity(xs.len() * ys.len() * zs.len());
    for x in &xs {
        for y in &ys {
            for z in &zs {
                out.push([*x, *y, *z]);
            }
        }
    }
    out
}

fn solve_normal(m: &mut [[f64; 5]; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let pivot = (col..4).max_by(|a, b| m[*a][col].abs().total_cmp(&m[*b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        let pivot_row = m[col];
        for (row, r) in m.iter_mut().enumerate() {
            if row == col {
                continue;
            }
            let f = r[col] / pivot_row[col];
            for (x, p) in r.iter_mut().zip(pivot_row.iter()).skip(col) {
                *x -= f * p;
            }
        }
    }
    Some([
        m[0][4] / m[0][0],
        m[1][4] / m[1][1],
        m[2][4] / m[2][2],
        m[3][4] / m[3][3],
    ])
}

pub fn fit_affine(src: &[[f64; 3]], dst: &[[f64; 3]]) -> Result<AffineFit, String> {
    if src.len() != dst.len() || src.len() < 4 {
        return Err(format!(
            "an affine fit needs at least 4 matching points, got {} and {}",
            src.len(),
            dst.len()
        ));
    }
    let mut gram = [[0.0f64; 4]; 4];
    let mut rhs = [[0.0f64; 4]; 3];
    for (p, q) in src.iter().zip(dst) {
        let h = [p[0], p[1], p[2], 1.0];
        for i in 0..4 {
            for j in 0..4 {
                gram[i][j] += h[i] * h[j];
            }
            for (c, r) in rhs.iter_mut().enumerate() {
                r[i] += h[i] * q[c];
            }
        }
    }
    let mut matrix: Mat4 = [[0.0, 0.0, 0.0, 0.0]; 4];
    matrix[3][3] = 1.0;
    for c in 0..3 {
        let mut aug = [[0.0f64; 5]; 4];
        for i in 0..4 {
            aug[i][..4].copy_from_slice(&gram[i]);
            aug[i][4] = rhs[c][i];
        }
        let x = solve_normal(&mut aug)
            .ok_or_else(|| "the affine fit is degenerate (points are coplanar)".to_owned())?;
        matrix[c] = x;
    }
    let sse: f64 = src
        .iter()
        .zip(dst)
        .map(|(p, q)| {
            let m = fastwarp_core::apply_rowmajor(&matrix, *p);
            (0..3).map(|a| (m[a] - q[a]).powi(2)).sum::<f64>()
        })
        .sum();
    Ok(AffineFit {
        matrix,
        rms_mm: (sse / src.len() as f64).sqrt(),
    })
}

impl Bridge {
    pub fn composite_path(&self) -> PathBuf {
        suffixed(&self.prefix, "Composite.h5")
    }

    pub fn inverse_composite_path(&self) -> PathBuf {
        suffixed(&self.prefix, "InverseComposite.h5")
    }

    fn reference_t1_from_record(&self) -> Option<PathBuf> {
        read_record(&self.dir)?
            .get("reference_t1")?
            .as_str()
            .map(PathBuf::from)
    }

    fn forward_chain(&self) -> Result<TransformChain, String> {
        let p = self.composite_path();
        read_transform_file(&p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// Target-space millimetres (RAS) to reference-space millimetres (RAS): the forward
    /// composite evaluated as a point transform.
    pub fn points_to_reference(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, String> {
        match self.forward_chain() {
            Ok(chain) => RustPointWarp::from_chain(chain)
                .to_template(pts)
                .map_err(point_err),
            Err(read_error) => {
                let reference = self.reference_t1_from_record().ok_or_else(|| {
                    format!("{read_error}; no reference template recorded for the fallback")
                })?;
                AntsPointWarp {
                    exe: crate::bundled_bin::find_binary("antsApplyTransforms"),
                    template: reference,
                    tokens: vec![
                        "-t".to_owned(),
                        self.composite_path().to_string_lossy().into_owned(),
                    ],
                    threads: None,
                    cancel: None,
                }
                .to_template(pts)
                .map_err(|e| format!("{read_error}; antsApplyTransforms fallback: {e}"))
            }
        }
    }

    pub fn affine_fit_to_reference(&self) -> Result<AffineFit, String> {
        let lattice = affine_lattice();
        let mapped = self.points_to_reference(&lattice)?;
        fit_affine(&lattice, &mapped)
    }

    /// Row-major 4x4 mapping target-space to reference-space millimetres (RAS), fitted by least
    /// squares to [`Bridge::points_to_reference`] on the lattice of [`affine_lattice`].
    pub fn affine_to_reference(&self) -> Result<Mat4, String> {
        self.affine_fit_to_reference().map(|f| f.matrix)
    }

    /// Resamples a reference-space image onto the grid of `target_grid` (a target-space image).
    pub fn warp_image_to_target(
        &self,
        src: &Path,
        target_grid: &Path,
        out: &Path,
        nearest: bool,
    ) -> Result<(), String> {
        let interpolation = if nearest {
            Interpolation::NearestNeighbor
        } else {
            Interpolation::Linear
        };
        match self.forward_chain() {
            Ok(chain) => apply_nifti(src, target_grid, &chain, interpolation, 0.0, out)
                .map_err(|e| format!("{}: {e}", src.display())),
            Err(read_error) => {
                let exe =
                    crate::bundled_bin::find_binary("antsApplyTransforms").ok_or_else(|| {
                        format!("{read_error}; antsApplyTransforms not found for the fallback")
                    })?;
                let mut cmd = std::process::Command::new(exe);
                cmd.args(["-d", "3", "-i"])
                    .arg(src)
                    .arg("-r")
                    .arg(target_grid)
                    .arg("-o")
                    .arg(out)
                    .arg("-n")
                    .arg(if nearest { "NearestNeighbor" } else { "Linear" })
                    .arg("-t")
                    .arg(self.composite_path());
                spawn_logged(&mut cmd, STAGE, None, None)
                    .map_err(|e| format!("{read_error}; antsApplyTransforms fallback: {e}"))
            }
        }
    }

    pub fn warp_images_to_target(
        &self,
        jobs: &[(PathBuf, PathBuf)],
        target_grid: &Path,
        nearest: bool,
    ) -> Result<(), String> {
        let Ok(chain) = self.forward_chain() else {
            for (src, out) in jobs {
                self.warp_image_to_target(src, target_grid, out, nearest)?;
            }
            return Ok(());
        };
        let interpolation = if nearest {
            Interpolation::NearestNeighbor
        } else {
            Interpolation::Linear
        };
        let voxels = fastwarp_reg::nifti_io::read_grid(target_grid)
            .map_err(|e| format!("{}: {e}", target_grid.display()))?
            .num_voxels()
            .max(1);
        let width = (WARP_MEMORY_BUDGET_BYTES / (voxels * WARP_BYTES_PER_VOXEL))
            .clamp(1, WARP_MAX_PARALLEL_IMAGES);
        for chunk in jobs.chunks(width) {
            chunk.par_iter().try_for_each(|(src, out)| {
                apply_nifti(src, target_grid, &chain, interpolation, 0.0, out)
                    .map_err(|e| format!("{}: {e}", src.display()))
            })?;
        }
        Ok(())
    }
}

pub fn reference_block(
    template_sides: &Map<String, Value>,
    bridge: &Bridge,
) -> Result<Map<String, Value>, String> {
    let sides = crate::ct_recon::warp_sides(template_sides, bridge).map_err(|e| e.to_string())?;
    let mut block = Map::new();
    block.insert("space".into(), json!(REFERENCE_SPACE));
    block.extend(sides);
    Ok(block)
}

impl PointWarp for Bridge {
    fn to_template(&self, pts: &[[f64; 3]]) -> Result<Vec<[f64; 3]>, PointWarpError> {
        self.points_to_reference(pts).map_err(PointWarpError::Io)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_reg::transform::{
        write_composite_pair, AffineTransform, CompositePair, TransformElem,
    };
    use fastwarp_reg::{lps_to_ras, Grid, Volume};

    const ID3: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbsloc_bridge_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn bridge_with(dir: &Path, affine: AffineTransform) -> Bridge {
        let inverse = affine.inverse().unwrap();
        let pair = CompositePair {
            forward: TransformChain::from_queue(vec![TransformElem::Affine(affine)]),
            inverse: TransformChain::from_queue(vec![TransformElem::Affine(inverse)]),
        };
        let prefix = bridge_prefix(dir);
        write_composite_pair(&prefix, &pair).unwrap();
        Bridge {
            dir: dir.to_path_buf(),
            target_space: "TestSpace".into(),
            prefix,
        }
    }

    fn blob_grid() -> Grid {
        Grid::new([48, 48, 48], [1.0; 3], [30.0, 30.0, -20.0], ID3).unwrap()
    }

    fn blob_at_index(c: [f64; 3]) -> Volume<f32> {
        let g = blob_grid();
        let data = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let d2 = (i as f64 - c[0]).powi(2)
                    + (j as f64 - c[1]).powi(2)
                    + (k as f64 - c[2]).powi(2);
                (-d2 / (2.0 * 2.5f64.powi(2))).exp() as f32
            })
            .collect();
        Volume::from_vec(g, data).unwrap()
    }

    fn centroid_ras(vol: &Volume<f32>) -> [f64; 3] {
        let g = vol.grid();
        let (mut sum, mut acc) = (0.0f64, [0.0f64; 3]);
        for (lin, v) in vol.data().iter().enumerate() {
            let w = f64::from(*v);
            let [i, j, k] = g.ijk(lin);
            let p = lps_to_ras(g.index_to_point([i as f64, j as f64, k as f64]));
            sum += w;
            for a in 0..3 {
                acc[a] += w * p[a];
            }
        }
        [acc[0] / sum, acc[1] / sum, acc[2] / sum]
    }

    #[test]
    fn points_and_images_move_in_the_same_direction() {
        let root = scratch("direction");
        let t_lps = [3.0, -2.0, 4.0];
        let bridge = bridge_with(&root, AffineTransform::from_translation(t_lps));
        let c_index = [24.0, 24.0, 24.0];
        let reference_img = blob_at_index(c_index);
        let ref_path = root.join("reference.nii.gz");
        reference_img.save_nifti(&ref_path).unwrap();
        let target_path = root.join("target.nii.gz");
        Volume::<f32>::zeros(blob_grid())
            .save_nifti(&target_path)
            .unwrap();
        let out = root.join("warped.nii.gz");
        bridge
            .warp_image_to_target(&ref_path, &target_path, &out, false)
            .unwrap();
        let warped = Volume::<f32>::load_nifti(&out).unwrap();
        let c_ras = centroid_ras(&reference_img);
        let q = centroid_ras(&warped);
        let t_ras = [-t_lps[0], -t_lps[1], t_lps[2]];
        let mapped = bridge.points_to_reference(&[q]).unwrap()[0];
        eprintln!("reference centre {c_ras:?}, warped centre {q:?}, p(q) {mapped:?}");
        for a in 0..3 {
            assert!(
                (mapped[a] - c_ras[a]).abs() < 0.5,
                "axis {a}: p(q) {} vs reference centre {}",
                mapped[a],
                c_ras[a]
            );
            assert!((q[a] - (c_ras[a] - t_ras[a])).abs() < 0.5, "axis {a}");
        }
        let p = [10.0, -20.0, 5.5];
        let m = bridge.points_to_reference(&[p]).unwrap()[0];
        for a in 0..3 {
            assert!((m[a] - (p[a] + t_ras[a])).abs() < 1e-4, "axis {a}");
        }
        let nearest_out = root.join("warped_nn.nii.gz");
        bridge
            .warp_image_to_target(&ref_path, &target_path, &nearest_out, true)
            .unwrap();
        let nn = Volume::<f32>::load_nifti(&nearest_out).unwrap();
        let q_nn = centroid_ras(&nn);
        for a in 0..3 {
            assert!((q_nn[a] - q[a]).abs() < 0.5, "axis {a}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn affine_fit_recovers_translation_and_scale() {
        let root = scratch("affine");
        let affine = AffineTransform::new(
            [[1.02, 0.0, 0.0], [0.0, 0.98, 0.0], [0.0, 0.0, 1.05]],
            [3.0, -2.0, 4.0],
            [0.0; 3],
        );
        let bridge = bridge_with(&root, affine);
        let fit = bridge.affine_fit_to_reference().unwrap();
        let want: Mat4 = [
            [1.02, 0.0, 0.0, -3.0],
            [0.0, 0.98, 0.0, 2.0],
            [0.0, 0.0, 1.05, 4.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        for (r, want_row) in want.iter().enumerate() {
            for (c, w) in want_row.iter().enumerate() {
                assert!(
                    (fit.matrix[r][c] - w).abs() < 1e-3,
                    "[{r}][{c}] {} vs {w}",
                    fit.matrix[r][c]
                );
            }
        }
        assert!(fit.rms_mm < 1e-3, "rms {}", fit.rms_mm);
        assert_eq!(bridge.affine_to_reference().unwrap(), fit.matrix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_reference_block_maps_every_marker_and_names_the_space() {
        let root = scratch("block");
        let bridge = bridge_with(&root, AffineTransform::from_translation([3.0, -2.0, 4.0]));
        let side = |x: f64| {
            json!({
                "coords_mm": [[x, 1.0, 2.0], [x, 1.0, 4.0]],
                "trajectory": [[x, 1.0, 2.0]],
                "markers": {"head": [x, 1.0, 2.0], "tail": [x, 1.0, 12.0]},
            })
        };
        let mut sides = Map::new();
        sides.insert("1".into(), side(10.0));
        sides.insert("2".into(), side(-10.0));
        let block = reference_block(&sides, &bridge).unwrap();
        assert_eq!(block["space"], json!(REFERENCE_SPACE));
        let head = &block["1"]["markers"]["head"];
        for (a, want) in [7.0, 3.0, 6.0].iter().enumerate() {
            assert!((head[a].as_f64().unwrap() - want).abs() < 1e-4, "axis {a}");
        }
        let left = &block["2"]["coords_mm"][1];
        for (a, want) in [-13.0, 3.0, 8.0].iter().enumerate() {
            assert!((left[a].as_f64().unwrap() - want).abs() < 1e-4, "axis {a}");
        }
        assert!(block["1"]["trajvector"].is_array());
        assert_eq!(block.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn xyz(v: &Value) -> [f64; 3] {
        let a = v.as_array().unwrap();
        [
            a[0].as_f64().unwrap(),
            a[1].as_f64().unwrap(),
            a[2].as_f64().unwrap(),
        ]
    }

    fn rows(v: &Value) -> Vec<[f64; 3]> {
        v.as_array().unwrap().iter().map(xyz).collect()
    }

    fn diff(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn length(a: [f64; 3]) -> f64 {
        dot(a, a).sqrt()
    }

    fn unit(a: [f64; 3]) -> [f64; 3] {
        let n = length(a);
        [a[0] / n, a[1] / n, a[2] / n]
    }

    fn close(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
        length(diff(a, b)) < tol
    }

    fn template_side(
        model: &dbsloc_reconstruct::ElectrodeGeometry,
        head: [f64; 3],
        tail: [f64; 3],
        roll: f64,
    ) -> Value {
        let r = 0.5 * model.lead_diameter_mm;
        let t = unit(diff(tail, head));
        let horizontal = unit(cross(t, [0.0, 0.0, 1.0]));
        let (s, c) = roll.sin_cos();
        let x_dir = unit([
            c * horizontal[0] + s * cross(t, horizontal)[0],
            c * horizontal[1] + s * cross(t, horizontal)[1],
            c * horizontal[2] + s * cross(t, horizontal)[2],
        ]);
        let y_dir = cross(t, x_dir);
        let x: [f64; 3] = std::array::from_fn(|a| head[a] + r * x_dir[a]);
        let y: [f64; 3] = std::array::from_fn(|a| head[a] + r * y_dir[a]);
        let contacts =
            dbsloc_reconstruct::native_frame::contacts_from_markers(model, head, tail, x, y)
                .unwrap();
        let warped: Vec<[f64; 3]> = contacts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let d = 0.05 * (i as f64 + 1.0);
                [p[0] + d, p[1] - 0.5 * d, p[2] + 0.25 * d]
            })
            .collect();
        json!({
            "coords_mm": contacts,
            "coords_mm_warped": warped,
            "trajectory": [head, tail],
            "markers": {"head": head, "tail": tail, "x": x, "y": y},
        })
    }

    #[test]
    fn the_reference_block_rebuilds_markers_and_contacts_under_a_sheared_bridge() {
        let name = "Boston Scientific Vercise Directed";
        let model = dbsloc_reconstruct::electrode_by_name(name).unwrap();
        let r = 0.5 * model.lead_diameter_mm;
        let root = scratch("reference_contacts");
        let affine = AffineTransform::new(
            [[1.08, 0.2, 0.0], [0.0, 0.95, 0.15], [0.1, 0.0, 1.12]],
            [3.0, -2.0, 4.0],
            [0.0; 3],
        );
        let bridge = bridge_with(&root, affine);
        let mut reco = json!({
            "props": {"1": {"elmodel": name}, "2": {"elmodel": name}},
            "meta": {"template_contacts": "from_markers"},
            "mni": {
                "space": "MNI152NLin2009bAsym",
                "1": template_side(model, [12.0, -14.0, -6.0], [14.0, -6.0, 18.0], 0.7),
                "2": template_side(model, [-11.0, -13.0, -7.0], [-13.0, -5.0, 17.0], 2.1),
            },
        });
        let mni_before = reco["mni"].clone();

        let mut mni = reco["mni"].as_object().unwrap().clone();
        mni.remove("space");
        for key in ["1", "2"] {
            let side = mni.get_mut(key).unwrap().as_object_mut().unwrap();
            let kept = side.remove("coords_mm_warped").unwrap();
            side.insert("coords_mm".into(), kept);
        }
        let block = reference_block(&mni, &bridge).unwrap();
        reco.as_object_mut()
            .unwrap()
            .insert("reference".into(), Value::Object(block));
        let mut lines = Vec::new();
        crate::template_contacts::recompute_template_contacts(
            &mut reco,
            "reference",
            None,
            &|_| crate::template_contacts::WarpedContacts::Capture,
            &mut |l| lines.push(l),
        );

        assert!(
            !lines
                .iter()
                .any(|l| l.contains("contacts kept as individually mapped points")),
            "{lines:?}"
        );
        assert_eq!(reco["mni"], mni_before);
        assert_eq!(reco["meta"]["template_contacts"], "from_markers");
        assert_eq!(reco["reference"]["space"], json!(REFERENCE_SPACE));

        for key in ["1", "2"] {
            let src = &mni_before[key];
            let side = &reco["reference"][key];
            let m = &side["markers"];
            let mapped = |v: &Value| bridge.points_to_reference(&[xyz(v)]).unwrap()[0];
            let head = xyz(&m["head"]);
            let tail = xyz(&m["tail"]);
            let x = xyz(&m["x"]);
            let y = xyz(&m["y"]);
            assert!(close(head, mapped(&src["markers"]["head"]), 1e-6), "{key}");
            assert!(close(tail, mapped(&src["markers"]["tail"]), 1e-6), "{key}");

            let axis = unit(diff(tail, head));
            let xv = diff(x, head);
            let yv = diff(y, head);
            assert!((length(xv) - r).abs() < 1e-6, "{key} |x| {}", length(xv));
            assert!((length(yv) - r).abs() < 1e-6, "{key} |y| {}", length(yv));
            assert!(dot(unit(xv), axis).abs() < 1e-9, "{key} x . axis");
            assert!(dot(unit(yv), axis).abs() < 1e-9, "{key} y . axis");
            assert!(dot(unit(xv), unit(yv)).abs() < 1e-9, "{key} x . y");

            let skewed = mapped(&src["markers"]["x"]);
            let skewed_dot = dot(unit(diff(skewed, head)), axis).abs();
            assert!(skewed_dot > 0.01, "{key} point-mapped x dot {skewed_dot}");

            let want =
                dbsloc_reconstruct::native_frame::contacts_from_markers(model, head, tail, x, y)
                    .unwrap();
            let got = rows(&side["coords_mm"]);
            assert_eq!(got.len(), want.len(), "{key}");
            assert_eq!(got.len(), model.n_contacts, "{key}");
            assert!(!got.is_empty(), "{key}");
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                assert!(close(*g, *w, 1e-9), "{key} contact {i}: {g:?} vs {w:?}");
            }

            let warped_src = rows(&src["coords_mm_warped"]);
            let warped = rows(&side["coords_mm_warped"]);
            let expected = bridge.points_to_reference(&warped_src).unwrap();
            assert_eq!(warped.len(), expected.len(), "{key}");
            for (i, (g, w)) in warped.iter().zip(expected.iter()).enumerate() {
                assert!(close(*g, *w, 1e-6), "{key} warped {i}: {g:?} vs {w:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fit_affine_refuses_degenerate_input() {
        let flat: Vec<[f64; 3]> = (0..10).map(|i| [i as f64, 2.0 * i as f64, 0.0]).collect();
        assert!(fit_affine(&flat, &flat).is_err());
        assert!(fit_affine(&flat[..2], &flat[..2]).is_err());
        assert!(fit_affine(&flat, &flat[..5]).is_err());
    }

    #[test]
    fn cache_is_valid_only_while_both_checksums_match() {
        let rec = json!({"reference_t1_sha256": "aa", "target_t1_sha256": "bb"});
        assert_eq!(
            cache_decision(Some(&rec), true, "aa", "bb"),
            CacheDecision::Use
        );
        assert_eq!(
            cache_decision(Some(&rec), true, "aa", "cc"),
            CacheDecision::Rebuild
        );
        assert_eq!(
            cache_decision(Some(&rec), true, "zz", "bb"),
            CacheDecision::Rebuild
        );
        assert_eq!(
            cache_decision(Some(&rec), false, "aa", "bb"),
            CacheDecision::Rebuild
        );
        assert_eq!(
            cache_decision(None, true, "aa", "bb"),
            CacheDecision::Rebuild
        );
        assert_eq!(
            cache_decision(Some(&json!({"target_t1_sha256": "bb"})), true, "aa", "bb"),
            CacheDecision::Rebuild
        );
    }

    #[test]
    fn directory_name_carries_the_space_and_the_checksum_prefix() {
        assert_eq!(
            bridge_dir_name("MNI152NLin2009cSym", "0123456789abcdef"),
            "MNI152NLin2009cSym__0123456789ab"
        );
        assert_eq!(bridge_dir_name("a/b..\\c", "ff"), "ab..c__ff");
        assert!(bridge_prefix(Path::new("d"))
            .to_string_lossy()
            .ends_with(&format!("from-{REFERENCE_SPACE}_")));
    }

    #[test]
    fn iso_timestamps_follow_the_civil_calendar() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(1_791_806_400), "2026-10-12T12:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn the_reference_space_needs_no_bridge() {
        let root = scratch("reference");
        let t1 = root.join("space/MNI152NLin2009bAsym/res-1mm/t1.nii.gz");
        std::fs::create_dir_all(t1.parent().unwrap()).unwrap();
        std::fs::write(&t1, b"x").unwrap();
        let opts = RunOptions::new(&root, "1");
        assert!(ensure_bridge(&t1, &opts, None, None).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn template_blobs(path: &Path, shift: f64) {
        let g = Grid::new([28, 28, 28], [1.0; 3], [0.0; 3], ID3).unwrap();
        let centres = [
            ([0.0, 0.0, 0.0], 5.0, 0.8),
            ([4.0, 2.0, -1.0], 2.5, 0.9),
            ([-4.0, 3.0, 2.0], 3.0, 0.7),
        ];
        let data: Vec<f32> = (0..g.num_voxels())
            .map(|lin| {
                let [i, j, k] = g.ijk(lin);
                let c = 13.5;
                let (x, y, z) = (i as f64 - c - shift, j as f64 - c, k as f64 - c);
                let noise = ((lin * 2_654_435_761) % 1000) as f64 / 20_000.0;
                let v: f64 = centres
                    .iter()
                    .map(|(b, s, a)| {
                        let d2 = (x - b[0]).powi(2) + (y - b[1]).powi(2) + (z - b[2]).powi(2);
                        a * (-d2 / (2.0 * s * s)).exp()
                    })
                    .sum();
                (v + noise) as f32
            })
            .collect();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Volume::from_vec(g, data).unwrap().save_nifti(path).unwrap();
    }

    #[test]
    fn bridge_between_two_templates_recovers_a_known_offset_on_the_cpu_engine() {
        let root = scratch("e2e");
        let reference = root.join("reference/t1.nii.gz");
        let target = root.join("target/t1.nii.gz");
        template_blobs(&reference, 0.0);
        template_blobs(&target, 2.0);
        let data_root = root.join("data");
        let src = BridgeSources {
            data_root: &data_root,
            reference_t1: &reference,
            target_t1: &target,
            target_space: "SyntheticSpace",
        };
        let mut opts = RunOptions::new(&root, "1");
        opts.norm_isotropic_mm = crate::options::NormGrid::Full;
        let mut choice = || {
            Ok(RegChoice {
                requested: crate::reg_backend::RegBackend::Auto,
                resolution: Resolution::Wgpu,
                engine: Some(Box::new(crate::reg_backend::CpuReferenceEngine) as Box<_>),
            })
        };
        let mut never = |_: &[String]| -> PipelineResult<()> {
            Err(PipelineError::Message("no external program may run".into()))
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge =
            ensure_bridge_with(&src, &opts, &mut choice, &mut never, None, Some(&tx)).unwrap();
        assert!(bridge.composite_path().is_file());
        assert!(bridge.inverse_composite_path().is_file());
        let rec = read_record(&bridge.dir).unwrap();
        assert_eq!(rec["target_space"], json!("SyntheticSpace"));
        assert_eq!(rec["channels"], json!(["T1w"]));
        assert_eq!(rec["backend"], json!("wgpu"));
        assert_eq!(rec["reference_space"], json!(REFERENCE_SPACE));
        let lines: Vec<String> = rx
            .try_iter()
            .filter_map(|e| match e {
                RunEvent::LogLine { line } => Some(line),
                _ => None,
            })
            .collect();
        assert!(lines
            .iter()
            .any(|l| l.contains("bridge: building MNI152NLin2009bAsym -> SyntheticSpace")));
        let leftovers = std::fs::read_dir(bridges_root(&data_root))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".partial-"))
            .count();
        assert_eq!(leftovers, 0);

        let c = 13.5;
        let ref_centre = lps_to_ras(
            Grid::new([28, 28, 28], [1.0; 3], [0.0; 3], ID3)
                .unwrap()
                .index_to_point([c, c, c]),
        );
        let target_centre = lps_to_ras(
            Grid::new([28, 28, 28], [1.0; 3], [0.0; 3], ID3)
                .unwrap()
                .index_to_point([c + 2.0, c, c]),
        );
        let mapped = bridge.points_to_reference(&[target_centre]).unwrap()[0];
        let err = (0..3)
            .map(|a| (mapped[a] - ref_centre[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        eprintln!(
            "target {target_centre:?} -> {mapped:?}, reference {ref_centre:?}, error {err:.3} mm"
        );
        assert!(err < 0.5, "offset error {err} mm");

        let (tx2, rx2) = std::sync::mpsc::channel();
        let again =
            ensure_bridge_with(&src, &opts, &mut choice, &mut never, None, Some(&tx2)).unwrap();
        assert_eq!(again, bridge);
        assert!(rx2.try_iter().any(|e| matches!(
            e,
            RunEvent::LogLine { line } if line.contains("using cached")
        )));

        template_blobs(&target, 3.0);
        let rebuilt = ensure_bridge_with(&src, &opts, &mut choice, &mut never, None, None).unwrap();
        let rec2 = read_record(&rebuilt.dir).unwrap();
        assert_ne!(rec2["target_t1_sha256"], rec["target_t1_sha256"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
