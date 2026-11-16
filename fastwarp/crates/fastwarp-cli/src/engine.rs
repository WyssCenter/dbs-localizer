// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The engine calls behind both front ends, the `fastwarp` command line and the Python extension
//! `fastwarp._core`: the same checks, the same GPU selection, the same outputs. Each front end
//! turns a [`Failure`] into its own form (an exit code, a Python exception).

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use fastwarp_gpu::{
    plan_capacity_linear, plan_capacity_syn, BackendSelect, CapacityCaps, CapacityError,
    WgpuBackend,
};
use fastwarp_reg::init::{initial_transform, InitFeature};
use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::preprocess::preprocess_volume;
use fastwarp_reg::register::RegisterTimings;
use fastwarp_reg::stages::{register_stages, write_stage_outputs, StagePlan, StagesOutput};
use fastwarp_reg::syn::SynOptions;
use fastwarp_reg::transform::ants_io::{with_suffix, COMPOSITE_SUFFIX};
use fastwarp_reg::transform::{
    read_transform_file, AffineTransform, TransformChain, TransformElem,
};
use fastwarp_reg::{Grid, RegError, Volume};

use crate::post;
use crate::recipe::{kind_name, Recipe};
use crate::session::Session;

/// Why an engine call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// An argument the caller got wrong, found before any work.
    Invalid(String),
    /// No usable GPU adapter.
    GpuUnavailable(String),
    /// The registration grid does not fit the GPU's memory budget or binding limit.
    GpuCapacity(String),
    /// The registration itself failed.
    Registration(String),
    /// The run was cancelled through its cancel flag.
    Cancelled,
    /// An input file (a transform) could not be read.
    Read(String),
    /// An output file could not be written.
    Write(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Cancelled => f.write_str("registration cancelled"),
            Failure::Invalid(m)
            | Failure::GpuUnavailable(m)
            | Failure::GpuCapacity(m)
            | Failure::Registration(m)
            | Failure::Read(m)
            | Failure::Write(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Failure {}

/// Runs engine code, turning a panic (a bug, never expected) into [`Failure::Registration`], so
/// a caller can still fall back to another engine.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, Failure>) -> Result<T, Failure> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Err(Failure::Registration(format!(
            "internal engine error (panic): {msg}"
        )))
    })
}

/// [`RegError::Cancelled`] as [`Failure::Cancelled`], anything else as a registration failure.
pub fn reg_err(e: RegError) -> Failure {
    match e {
        RegError::Cancelled => Failure::Cancelled,
        other => Failure::Registration(other.to_string()),
    }
}

/// Voxels per axis of the registration grid: `resample_isotropic`'s rule on the first fixed image.
pub fn iso_dims(grid: &Grid, mm: f64) -> [usize; 3] {
    let (size, spacing) = (grid.size(), grid.spacing());
    let mut out = [1usize; 3];
    for a in 0..3 {
        if (spacing[a] - mm).abs() < 1e-6 {
            out[a] = size[a];
        } else {
            let last = size[a].saturating_sub(1) as f64;
            out[a] = (((last * spacing[a] / mm) + 1e-9).floor() as usize + 1).max(1);
        }
    }
    out
}

/// The default registration grid spacing of the command line: the smallest voxel spacing of
/// the (first) fixed image. The Python functions default to 1.0 mm instead.
pub fn finest_spacing(grid: &Grid) -> f64 {
    grid.spacing().into_iter().fold(f64::INFINITY, f64::min)
}

/// The GPU the environment (`FASTWARP_GPU_BACKEND`, `FASTWARP_GPU_ADAPTER`,
/// `FASTWARP_GPU_ALLOW_SOFTWARE`, `WGPU_FORCE_FALLBACK_ADAPTER`) selects; `adapter_substring`
/// overrides `FASTWARP_GPU_ADAPTER`.
pub fn open_gpu(adapter_substring: Option<String>) -> Result<WgpuBackend, Failure> {
    let mut sel = BackendSelect::from_env().map_err(|e| Failure::GpuUnavailable(e.to_string()))?;
    if adapter_substring.is_some() {
        sel.adapter_substring = adapter_substring;
    }
    WgpuBackend::new(&sel).map_err(|e| Failure::GpuUnavailable(e.to_string()))
}

/// `name (backend, device type)` of the GPU in use.
pub fn adapter_name(g: &WgpuBackend) -> String {
    let i = &g.context().info;
    format!("{} ({}, {})", i.name, i.backend, i.device_type)
}

/// Refuses an output prefix whose folder does not exist.
pub fn check_out_folder(out_prefix: &Path) -> Result<(), Failure> {
    match with_suffix(out_prefix, COMPOSITE_SUFFIX).parent() {
        Some(dir) if !dir.as_os_str().is_empty() && !dir.is_dir() => Err(Failure::Invalid(
            format!("output folder {} does not exist", dir.display()),
        )),
        _ => Ok(()),
    }
}

/// Refuses an `iso_mm` that is not a positive number.
pub fn check_iso_mm(iso_mm: f64) -> Result<(), Failure> {
    if iso_mm.is_finite() && iso_mm > 0.0 {
        Ok(())
    } else {
        Err(Failure::Invalid("iso_mm must be a positive number".into()))
    }
}

/// Refuses anything but 1 or 2 fixed images and as many moving images.
pub fn check_channels(fixed: usize, moving: usize) -> Result<(), Failure> {
    if fixed == 0 || fixed > 2 || fixed != moving {
        return Err(Failure::Invalid(
            "give 1 or 2 fixed images and as many moving images".into(),
        ));
    }
    Ok(())
}

/// Refuses winsorize quantiles outside `0 <= lower < upper <= 1`.
pub fn check_winsorize(winsorize: Option<(f32, f32)>) -> Result<(), Failure> {
    match winsorize {
        Some((lo, hi)) if !(0.0 <= lo && lo < hi && hi <= 1.0) => Err(Failure::Invalid(
            "winsorize must be (lower, upper) quantiles with 0 <= lower < upper <= 1".into(),
        )),
        _ => Ok(()),
    }
}

pub use fastwarp_reg::stages::OutputLayout;

/// The layout of an `outputs` name: `composite`, `split` or `both`.
pub fn output_layout(name: &str) -> Result<OutputLayout, Failure> {
    match name {
        "composite" => Ok(OutputLayout::Composite),
        "split" => Ok(OutputLayout::Split),
        "both" => Ok(OutputLayout::Both),
        other => Err(Failure::Invalid(format!(
            "outputs must be 'composite', 'split' or 'both', not {other:?}"
        ))),
    }
}

/// Where [`register_syn`] writes: under `prefix` in `layout` (`Composite`: `Composite.h5`,
/// `InverseComposite.h5`, `0GenericAffine.mat`; `Split`: `0GenericAffine.mat` and, after a SyN
/// stage, `1Warp.nii.gz` / `1InverseWarp.nii.gz`; `Both`) and, with `write_warped`,
/// `<prefix>_Warped.nii.gz`.
#[derive(Clone, Debug)]
pub struct SynOutputs {
    pub prefix: PathBuf,
    pub write_warped: bool,
    pub layout: OutputLayout,
}

/// One linear stage of a run: kind (`rigid` / `affine`), iterations per level, final metric.
#[derive(Clone, Debug)]
pub struct StageSummary {
    pub transform: &'static str,
    pub iterations: Vec<u32>,
    pub metric: Option<f64>,
}

/// One SyN level of a run: level, grid, iterations, converged, last energy.
#[derive(Clone, Debug)]
pub struct LevelSummary {
    pub level: usize,
    pub grid_size: [usize; 3],
    pub iterations: usize,
    pub converged: bool,
    pub energy: f64,
}

/// What [`register_syn`] reports.
#[derive(Clone, Debug)]
pub struct SynRun {
    pub recipe: Recipe,
    pub adapter: String,
    pub grid: [usize; 3],
    /// From opening the GPU to the outputs written.
    pub total_s: f64,
    pub write_s: f64,
    pub timings: RegisterTimings,
    /// SyN levels; empty without a SyN stage.
    pub levels: Vec<LevelSummary>,
    /// The collapsed affine (start and linear stages), what `0GenericAffine.mat` holds.
    pub affine_parameters: [f64; 12],
    pub affine_center: [f64; 3],
    /// The start of the chain.
    pub initial_parameters: [f64; 12],
    pub initial_center: [f64; 3],
    /// The linear stages, in order.
    pub linear_stages: Vec<StageSummary>,
    /// Whether a SyN stage ran.
    pub deformable: bool,
    /// One message per linear level that was skipped because its samples left the metric
    /// undefined (the run went on with the finer levels).
    pub warnings: Vec<String>,
    /// The files written, in the order composites, affine, warps, warped image.
    pub files: Vec<PathBuf>,
    pub peak_gpu_bytes: u64,
    pub memory_budget_bytes: u64,
}

/// The pre-flight capacity check of every stage of `plan` for a `dims` registration grid and a
/// `moving_dims` moving grid: the linear stages (their sample buffers summed over the chain) and,
/// when there is one, the SyN stage. The idle linear buffers are freed before the SyN stage, so the
/// two are separate checks.
pub fn check_plan_capacity(
    plan: &StagePlan,
    dims: [usize; 3],
    moving_dims: [usize; 3],
    channels: usize,
    caps: &CapacityCaps,
) -> Result<(), CapacityError> {
    if !plan.linear.is_empty() {
        plan_capacity_linear(dims, moving_dims, &plan.linear, caps)?;
    }
    if let Some(syn) = &plan.syn {
        plan_capacity_syn(dims, moving_dims, channels, &syn.shrink_factors, caps)?;
    }
    Ok(())
}

/// Registers `moving` to `fixed` (1 or 2 channels each, checked by the caller with
/// [`check_channels`]) with `recipe` on the GPU, the registration grid isotropic at `iso_mm`.
/// Checks the capacity of every stage first. Writes `outputs` when given.
pub fn register_syn(
    fixed: Vec<Volume<f64>>,
    moving: Vec<Volume<f64>>,
    outputs: Option<SynOutputs>,
    iso_mm: f64,
    adapter: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
    recipe: Recipe,
) -> Result<SynRun, Failure> {
    register_with(
        fixed,
        moving,
        iso_mm,
        adapter,
        cancel,
        recipe,
        move |out, fixed, moving| {
            drop((fixed, moving));
            let t = Instant::now();
            let files = match &outputs {
                Some(o) => write_stage_outputs(&o.prefix, out, o.layout, o.write_warped)
                    .map_err(|e| Failure::Write(format!("writing outputs: {e}")))?,
                None => Vec::new(),
            };
            Ok((files, t.elapsed().as_secs_f64()))
        },
    )
}

/// [`register_syn`] with the outputs written by `write`, which gets the result and the input
/// images back (to resample them through the result) and returns the paths it wrote and the
/// seconds the writing took (`SynRun::write_s`).
pub fn register_with<W>(
    fixed: Vec<Volume<f64>>,
    moving: Vec<Volume<f64>>,
    iso_mm: f64,
    adapter: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
    recipe: Recipe,
    write: W,
) -> Result<SynRun, Failure>
where
    W: FnOnce(
        &StagesOutput,
        Vec<Volume<f64>>,
        Vec<Volume<f64>>,
    ) -> Result<(Vec<PathBuf>, f64), Failure>,
{
    guarded(move || {
        let t_all = Instant::now();
        let g = open_gpu(adapter)?;
        let caps = CapacityCaps::from_context(g.context());
        let dims = iso_dims(fixed[0].grid(), iso_mm);
        let moving_dims = moving
            .iter()
            .map(|m| iso_dims(m.grid(), iso_mm))
            .max_by_key(|d| d.iter().map(|n| *n as u128).product::<u128>())
            .unwrap_or(dims);
        check_plan_capacity(&recipe.plan, dims, moving_dims, fixed.len(), &caps)
            .map_err(|e| Failure::GpuCapacity(format!("{e} (at {iso_mm} mm)")))?;
        let options = SynOptions {
            cancel: cancel.as_deref(),
        };
        let out = register_stages(
            &g,
            &g,
            &recipe.plan,
            &fixed,
            &moving,
            Some(iso_mm),
            &options,
        )
        .map_err(reg_err)?;
        let (files, write_s) = write(&out, fixed, moving)?;
        let linear_stages = recipe
            .plan
            .linear
            .iter()
            .zip(&out.linear)
            .map(|(r, s)| StageSummary {
                transform: kind_name(r.kind),
                iterations: s.levels.iter().map(|l| l.cg.iterations).collect(),
                metric: s.final_metric(),
            })
            .collect();
        let warnings = recipe
            .plan
            .linear
            .iter()
            .zip(&out.linear)
            .enumerate()
            .flat_map(|(i, (r, s))| {
                s.warnings(&format!("linear stage {} ({})", i + 1, kind_name(r.kind)))
            })
            .collect();
        Ok(SynRun {
            recipe,
            adapter: adapter_name(&g),
            grid: out.grid.size(),
            total_s: t_all.elapsed().as_secs_f64(),
            write_s,
            timings: out.timings,
            levels: out
                .levels
                .iter()
                .map(|l| LevelSummary {
                    level: l.level,
                    grid_size: l.grid_size,
                    iterations: l.iterations.len(),
                    converged: l.converged,
                    energy: l.iterations.last().map(|i| i.energy).unwrap_or(0.0),
                })
                .collect(),
            affine_parameters: out.total.parameters(),
            affine_center: out.total.center,
            initial_parameters: out.initial.parameters(),
            initial_center: out.initial.center,
            linear_stages,
            deformable: out.deformable,
            warnings,
            files,
            peak_gpu_bytes: g.peak_allocated_bytes(),
            memory_budget_bytes: caps.memory_budget,
        })
    })
}

/// The GPU the engine would use.
#[derive(Clone, Debug)]
pub struct GpuInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub memory_budget_bytes: u64,
}

/// Opens the GPU the engine would use and describes it.
pub fn gpu_info(adapter: Option<String>) -> Result<GpuInfo, Failure> {
    let g = open_gpu(adapter)?;
    let i = &g.context().info;
    let caps = CapacityCaps::from_context(g.context());
    Ok(GpuInfo {
        name: i.name.clone(),
        backend: i.backend.clone(),
        device_type: i.device_type.clone(),
        driver: i.driver.clone(),
        memory_budget_bytes: caps.memory_budget,
    })
}

/// Whether a registration of a `dims` fixed grid and a `moving_dims` moving grid (default: the
/// fixed grid) with `channels` channels and the stages of `recipe` fits the GPU, as
/// [`register_syn`] checks it.
pub fn fits(
    dims: [usize; 3],
    moving_dims: Option<[usize; 3]>,
    channels: usize,
    adapter: Option<String>,
    recipe: &Recipe,
) -> Result<bool, Failure> {
    Ok(capacity_check(dims, moving_dims, channels, adapter, recipe)?.is_ok())
}

/// The capacity check of [`fits`], with the reason when the grid does not fit.
pub fn capacity_check(
    dims: [usize; 3],
    moving_dims: Option<[usize; 3]>,
    channels: usize,
    adapter: Option<String>,
    recipe: &Recipe,
) -> Result<Result<(), String>, Failure> {
    let g = open_gpu(adapter)?;
    let caps = CapacityCaps::from_context(g.context());
    Ok(check_plan_capacity(
        &recipe.plan,
        dims,
        moving_dims.unwrap_or(dims),
        channels,
        &caps,
    )
    .map_err(|e| e.to_string()))
}

/// The chain `ants.apply_transforms(transformlist=transforms, whichtoinvert=invert)` and
/// `antsApplyTransforms -t transforms[0] -t transforms[1] ...` apply: a reference point goes
/// through `transforms[0]` first (measured against ANTsPy, see `tests/test_post.py`). The queue
/// applies its last element first, so the files are pushed in reverse. A file whose `invert` flag
/// is set is replaced by its inverse in place (`-t [file,1]`); only linear transforms have one,
/// so inverting a displacement field is refused.
pub fn read_chain(
    transforms: &[PathBuf],
    invert: Option<&[bool]>,
) -> Result<TransformChain, Failure> {
    if transforms.is_empty() {
        return Err(Failure::Invalid("give at least one transform file".into()));
    }
    if let Some(flags) = invert {
        if flags.len() != transforms.len() {
            return Err(Failure::Invalid(format!(
                "invert has {} flags for {} transforms",
                flags.len(),
                transforms.len()
            )));
        }
    }
    let mut chain = TransformChain::new();
    for (i, path) in transforms.iter().enumerate().rev() {
        let mut part = read_transform_file(path)
            .map_err(|e| Failure::Read(format!("reading {}: {e}", path.display())))?;
        if invert.is_some_and(|f| f[i]) {
            let field = part
                .queue()
                .iter()
                .any(|t| matches!(t, TransformElem::DisplacementField(_)));
            part = part.inverse().map_err(|e| {
                Failure::Invalid(if field {
                    format!(
                        "cannot invert the displacement field in {}: pass the matching \
                         InverseWarp / InverseComposite file",
                        path.display()
                    )
                } else {
                    format!("cannot invert {}: {e}", path.display())
                })
            })?;
        }
        chain.extend(part);
    }
    Ok(chain)
}

/// Refuses a gzip level above 9.
pub fn check_compression_level(level: Option<u32>) -> Result<(), Failure> {
    if level.is_some_and(|l| l > 9) {
        return Err(Failure::Invalid("compression_level must be 0 to 9".into()));
    }
    Ok(())
}

/// Writes the transforms as one displacement field on `grid` (`antsApplyTransforms --compose`);
/// `invert` as in [`read_chain`].
pub fn compose(
    transforms: &[PathBuf],
    invert: Option<&[bool]>,
    grid: &Grid,
    out_path: &Path,
    compression_level: Option<u32>,
) -> Result<(), Failure> {
    check_compression_level(compression_level)?;
    guarded(|| {
        let chain = read_chain(transforms, invert)?;
        compose_chain(&chain, grid, out_path, compression_level)
    })
}

/// [`compose`] of a chain already read: the field is computed, then the output's folder is
/// created when missing and the field written.
pub fn compose_chain(
    chain: &TransformChain,
    grid: &Grid,
    out_path: &Path,
    compression_level: Option<u32>,
) -> Result<(), Failure> {
    check_compression_level(compression_level)?;
    guarded(|| {
        let field = post::compose_field(chain, grid).map_err(reg_err)?;
        if let Some(dir) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| {
                Failure::Write(format!("cannot create the folder {}: {e}", dir.display()))
            })?;
        }
        post::write_field(out_path, &field, compression_level)
            .map_err(|e| Failure::Write(format!("writing {}: {e}", out_path.display())))
    })
}

/// `image` resampled onto `grid` through the transforms (`antsApplyTransforms`, `invert` as in
/// [`read_chain`]); points that map outside the image take `default_value`. The voxels are
/// `f32`, x fastest.
pub fn apply(
    image: &Volume<f64>,
    grid: &Grid,
    transforms: &[PathBuf],
    invert: Option<&[bool]>,
    interp: post::Interp,
    default_value: f64,
) -> Result<Vec<f32>, Failure> {
    guarded(|| {
        let chain = read_chain(transforms, invert)?;
        apply_chain(image, grid, &chain, interp, default_value)
    })
}

/// [`apply`] through a chain already read (so a caller can refuse the transforms before it loads
/// the image).
pub fn apply_chain(
    image: &Volume<f64>,
    grid: &Grid,
    chain: &TransformChain,
    interp: post::Interp,
    default_value: f64,
) -> Result<Vec<f32>, Failure> {
    guarded(|| post::resample(image, grid, chain, interp, default_value).map_err(reg_err))
}

/// The interpolator of an ANTs name, or why the name is refused.
pub fn interpolator(name: &str) -> Result<post::Interp, Failure> {
    post::Interp::from_ants_name(name).ok_or_else(|| {
        Failure::Invalid(format!(
            "interpolation {name:?} is not supported (linear, nearestNeighbor, \
             lanczosWindowedSinc)"
        ))
    })
}

/// A prepared linear session: the pair converted, winsorized and on the GPU, with its
/// centre-of-mass start and the adapter name.
pub struct LinearStart {
    pub session: Session,
    pub com: AffineTransform,
    pub adapter: String,
}

/// Opens the GPU and prepares `fixed` / `moving` for linear fits: the centre-of-mass start on the
/// images as given (as ANTs initialises), then winsorizing (`(lower, upper)` quantiles, rescaled
/// to [0, 1]).
pub fn open_linear(
    fixed: Volume<f64>,
    moving: Volume<f64>,
    mask: Option<MaskVolume>,
    winsorize: Option<(f32, f32)>,
    adapter: Option<String>,
) -> Result<LinearStart, Failure> {
    check_winsorize(winsorize)?;
    guarded(move || {
        let gpu = open_gpu(adapter)?;
        let com = initial_transform(&fixed, &moving, InitFeature::CentreOfMass).map_err(reg_err)?;
        let (fixed, moving) = match winsorize {
            Some((lo, hi)) => (
                preprocess_volume(&fixed, lo, hi, 0.0, 1.0).map_err(reg_err)?,
                preprocess_volume(&moving, lo, hi, 0.0, 1.0).map_err(reg_err)?,
            ),
            None => (fixed, moving),
        };
        let adapter = adapter_name(&gpu);
        Ok(LinearStart {
            session: Session {
                gpu,
                fixed,
                moving,
                mask,
                cache: Default::default(),
            },
            com,
            adapter,
        })
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use fastwarp_reg::linalg::IDENTITY3;

    #[test]
    fn iso_dims_keeps_a_matching_spacing_and_floors_otherwise() {
        let g = Grid::new([10, 20, 5], [1.0, 0.5, 2.0], [0.0; 3], IDENTITY3).unwrap();
        assert_eq!(iso_dims(&g, 1.0), [10, 10, 9]);
        assert_eq!(iso_dims(&g, 0.5), [19, 20, 17]);
    }

    #[test]
    fn the_default_grid_is_the_finest_fixed_spacing() {
        let g = Grid::new([10, 20, 5], [1.2, 0.7, 3.0], [0.0; 3], IDENTITY3).unwrap();
        assert_eq!(finest_spacing(&g), 0.7);
    }

    #[test]
    fn argument_checks() {
        assert!(check_channels(1, 1).is_ok());
        assert!(check_channels(2, 2).is_ok());
        assert!(matches!(check_channels(2, 1), Err(Failure::Invalid(_))));
        assert!(matches!(check_channels(3, 3), Err(Failure::Invalid(_))));
        assert!(check_iso_mm(0.5).is_ok());
        assert!(check_iso_mm(0.0).is_err());
        assert!(check_iso_mm(f64::NAN).is_err());
        assert!(check_winsorize(Some((0.005, 0.995))).is_ok());
        assert!(check_winsorize(Some((0.5, 0.5))).is_err());
        assert!(check_winsorize(None).is_ok());
        assert!(check_compression_level(Some(9)).is_ok());
        assert!(check_compression_level(Some(10)).is_err());
        assert!(interpolator("nearestNeighbor").is_ok());
        assert!(interpolator("bspline").is_err());
        assert!(matches!(read_chain(&[], None), Err(Failure::Invalid(_))));
        assert!(matches!(
            read_chain(&[PathBuf::from("no/such/file.mat")], None),
            Err(Failure::Read(_))
        ));
        assert!(matches!(
            read_chain(&[PathBuf::from("a.mat")], Some(&[true, false])),
            Err(Failure::Invalid(_))
        ));
        assert!(output_layout("split").is_ok());
        assert!(matches!(output_layout("tar"), Err(Failure::Invalid(_))));
    }

    #[test]
    fn a_missing_output_folder_is_refused() {
        assert!(check_out_folder(Path::new("no_such_folder_fastwarp/sub_")).is_err());
        assert!(check_out_folder(Path::new("sub_")).is_ok());
    }

    #[test]
    fn an_inverted_affine_is_its_inverse_and_an_inverted_field_is_refused() {
        use fastwarp_reg::transform::field_nifti::write_field_nifti;
        use fastwarp_reg::transform::itk_mat::write_mat_file;
        use fastwarp_reg::DisplacementField;
        let d = std::env::temp_dir().join(format!("fastwarp_cli_engine_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = AffineTransform::new(
            [[1.05, 0.04, 0.0], [-0.03, 0.97, 0.02], [0.01, 0.0, 1.02]],
            [1.3, -2.1, 0.7],
            [2.0, 30.0, 10.0],
        );
        let shift = AffineTransform::from_translation([4.0, 0.0, -1.0]);
        let (pa, ps) = (d.join("a.mat"), d.join("shift.mat"));
        write_mat_file(&pa, &[TransformElem::Affine(a)]).unwrap();
        write_mat_file(&ps, &[TransformElem::Affine(shift)]).unwrap();
        let chain = read_chain(&[pa.clone(), ps.clone()], Some(&[true, false])).unwrap();
        let inv = a.inverse().unwrap();
        for p in [[0.0, 0.0, 0.0], [12.0, -5.0, 33.0]] {
            let want = shift.apply(inv.apply(p));
            let got = chain.apply(p);
            for k in 0..3 {
                assert!((got[k] - want[k]).abs() < 1e-12, "{got:?} {want:?}");
            }
        }
        let plain = read_chain(std::slice::from_ref(&pa), None).unwrap();
        assert_eq!(
            plain,
            read_chain(std::slice::from_ref(&pa), Some(&[false])).unwrap()
        );
        let grid = Grid::new(
            [3, 3, 3],
            [1.0; 3],
            [0.0; 3],
            fastwarp_reg::linalg::IDENTITY3,
        )
        .unwrap();
        let pf = d.join("1Warp.nii.gz");
        write_field_nifti(&pf, &DisplacementField::zeros(grid)).unwrap();
        match read_chain(&[pa, pf], Some(&[false, true])) {
            Err(Failure::Invalid(m)) => {
                assert!(m.contains("cannot invert the displacement field"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_panic_becomes_a_registration_failure() {
        let r: Result<(), Failure> = guarded(|| panic!("boom"));
        assert_eq!(
            r,
            Err(Failure::Registration(
                "internal engine error (panic): boom".into()
            ))
        );
    }
}
