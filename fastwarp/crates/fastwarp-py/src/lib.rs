// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Python extension `fastwarp._core`: the dbs-localizer cohort registration (centre of mass,
//! Rigid, Affine, SyN) on the GPU, with ANTs-compatible `Composite.h5` / `InverseComposite.h5`.
//!
//! Images arrive as flat `float32` or `float64` arrays in ITK voxel order (x fastest) with ITK
//! (LPS) geometry, i.e. exactly what an `ANTsImage` holds; the Python wrapper does the flattening
//! (a view, no copy, for an `ANTsImage`). They are converted to the engine's `f64` once, here.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod post;
mod recipe;
mod session;

use fastwarp_gpu::{plan_capacity, plan_capacity_pair, BackendSelect, CapacityCaps, WgpuBackend};
use fastwarp_reg::init::{initial_transform, InitFeature};
use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::preprocess::preprocess_volume;
use fastwarp_reg::register::{register_full, write_linear_output, write_outputs, RegisterTimings};
use fastwarp_reg::syn::SynOptions;
use fastwarp_reg::transform::AffineTransform;
use fastwarp_reg::transform::{read_transform_file, write_composite_pair, TransformChain};
use fastwarp_reg::{Grid, Mat3, RegError, Vec3, Volume};
use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

create_exception!(
    _core,
    GpuUnavailableError,
    PyRuntimeError,
    "No usable hardware GPU adapter (software adapters are refused unless FASTWARP_GPU_ALLOW_SOFTWARE=1)."
);
create_exception!(
    _core,
    GpuCapacityError,
    PyRuntimeError,
    "The registration grid does not fit the GPU's memory budget or binding limit."
);
create_exception!(
    _core,
    RegistrationError,
    PyRuntimeError,
    "The registration failed."
);
create_exception!(
    _core,
    RegistrationCancelled,
    PyException,
    "The registration was cancelled through its CancelToken."
);

/// A flag another thread sets to stop a running registration at its next iteration.
#[pyclass(module = "fastwarp", frozen)]
struct CancelToken {
    flag: Arc<AtomicBool>,
}

#[pymethods]
impl CancelToken {
    #[new]
    fn new() -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Asks the registration using this token to stop; it raises `RegistrationCancelled`.
    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    #[getter]
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// One image as handed over from Python: flat data (x fastest, `float32` or `float64`), size,
/// spacing, origin, direction.
type ImageArgs<'py> = (Bound<'py, PyAny>, [usize; 3], Vec3, Vec3, Mat3);

fn contiguous_err() -> PyErr {
    PyValueError::new_err("image data must be a contiguous 1-D array")
}

fn to_volume(img: &ImageArgs<'_>) -> PyResult<Volume<f64>> {
    let (data, size, spacing, origin, direction) = img;
    let grid = Grid::new(*size, *spacing, *origin, *direction)
        .map_err(|e| PyValueError::new_err(format!("bad image geometry: {e}")))?;
    let values: Vec<f64> = if let Ok(a) = data.extract::<PyReadonlyArray1<'_, f32>>() {
        let s = a.as_slice().map_err(|_| contiguous_err())?;
        s.iter().map(|&v| f64::from(v)).collect()
    } else if let Ok(a) = data.extract::<PyReadonlyArray1<'_, f64>>() {
        a.as_slice().map_err(|_| contiguous_err())?.to_vec()
    } else {
        let what = data
            .getattr("dtype")
            .map(|d| d.to_string())
            .unwrap_or_else(|_| data.get_type().to_string());
        return Err(PyValueError::new_err(format!(
            "image data must be a 1-D float32 or float64 array, got {what}"
        )));
    };
    Volume::from_vec(grid, values).map_err(|e| PyValueError::new_err(format!("bad image: {e}")))
}

/// Runs engine code, turning a panic (a bug, never expected) into `RegistrationError`, so a
/// caller's `except Exception` can still fall back to another engine.
fn guarded<T>(f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        Err(RegistrationError::new_err(format!(
            "internal engine error (panic): {msg}"
        )))
    })
}

/// Voxels per axis of the registration grid: `resample_isotropic`'s rule on the first fixed image.
fn iso_dims(grid: &Grid, mm: f64) -> [usize; 3] {
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

fn open_gpu(adapter_substring: Option<String>) -> PyResult<WgpuBackend> {
    let mut sel =
        BackendSelect::from_env().map_err(|e| GpuUnavailableError::new_err(e.to_string()))?;
    if adapter_substring.is_some() {
        sel.adapter_substring = adapter_substring;
    }
    WgpuBackend::new(&sel).map_err(|e| GpuUnavailableError::new_err(e.to_string()))
}

fn reg_err(e: RegError) -> PyErr {
    match e {
        RegError::Cancelled => RegistrationCancelled::new_err("registration cancelled"),
        other => RegistrationError::new_err(other.to_string()),
    }
}

struct Run {
    recipe: recipe::Recipe,
    adapter: String,
    grid: [usize; 3],
    total_s: f64,
    write_s: f64,
    timings: RegisterTimings,
    levels: Vec<(usize, [usize; 3], usize, bool, f64)>,
    affine_parameters: Option<[f64; 12]>,
    affine_center: Option<[f64; 3]>,
    peak_gpu_bytes: u64,
    memory_budget_bytes: u64,
}

fn adapter_name(g: &WgpuBackend) -> String {
    let i = &g.context().info;
    format!("{} ({}, {})", i.name, i.backend, i.device_type)
}

/// Registers `moving` to `fixed` (1 or 2 channels each) with the dbs-localizer cohort recipe on
/// the GPU and writes `<out_prefix>Composite.h5`, `<out_prefix>InverseComposite.h5` and
/// `<out_prefix>0GenericAffine.mat` (and, with `write_warped`, `<out_prefix>_Warped.nii.gz`).
/// Returns a dict of timings and diagnostics. The GIL is released while it runs.
#[pyfunction]
#[pyo3(signature = (fixed, moving, out_prefix, iso_mm=1.0, write_warped=false, adapter=None, cancel=None, recipe=None))]
#[allow(clippy::too_many_arguments)]
fn register_syn<'py>(
    py: Python<'py>,
    fixed: Vec<ImageArgs<'py>>,
    moving: Vec<ImageArgs<'py>>,
    out_prefix: PathBuf,
    iso_mm: f64,
    write_warped: bool,
    adapter: Option<String>,
    cancel: Option<PyRef<'py, CancelToken>>,
    recipe: Option<Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyDict>> {
    if fixed.is_empty() || fixed.len() > 2 || fixed.len() != moving.len() {
        return Err(PyValueError::new_err(
            "give 1 or 2 fixed images and as many moving images",
        ));
    }
    // Checked before any work, like everything a caller can get wrong.
    let recipe = recipe::Recipe::from_dict(recipe.as_ref(), fixed.len())?;
    if !(iso_mm.is_finite() && iso_mm > 0.0) {
        return Err(PyValueError::new_err("iso_mm must be a positive number"));
    }
    // Checked up front: otherwise a missing folder only shows when the outputs are written.
    match out_prefix.parent() {
        Some(dir) if !dir.as_os_str().is_empty() && !dir.is_dir() => {
            return Err(PyValueError::new_err(format!(
                "output folder {} does not exist",
                dir.display()
            )));
        }
        _ => {}
    }
    let fixed: Vec<Volume<f64>> = fixed.iter().map(to_volume).collect::<PyResult<_>>()?;
    let moving: Vec<Volume<f64>> = moving.iter().map(to_volume).collect::<PyResult<_>>()?;
    let flag = cancel.map(|c| Arc::clone(&c.flag));

    let run = py.detach(move || {
        guarded(move || -> PyResult<Run> {
            let t_all = Instant::now();
            let g = open_gpu(adapter)?;
            let caps = CapacityCaps::from_context(g.context());
            let dims = iso_dims(fixed[0].grid(), iso_mm);
            let moving_dims = moving
                .iter()
                .map(|m| iso_dims(m.grid(), iso_mm))
                .max_by_key(|d| d.iter().map(|n| *n as u128).product::<u128>())
                .unwrap_or(dims);
            plan_capacity_pair(dims, moving_dims, fixed.len(), &caps)
                .map_err(|e| GpuCapacityError::new_err(format!("{e} (at {iso_mm} mm)")))?;
            let options = SynOptions {
                cancel: flag.as_deref(),
            };
            let out = register_full(
                &g,
                &g,
                &recipe.linear,
                &recipe.syn,
                &fixed,
                &moving,
                Some(iso_mm),
                &options,
            )
            .map_err(reg_err)?;
            drop((fixed, moving));
            let t = Instant::now();
            let io = |e: RegError| RegistrationError::new_err(format!("writing outputs: {e}"));
            if write_warped {
                write_outputs(&out_prefix, &out).map_err(io)?;
            } else {
                write_composite_pair(&out_prefix, &out.pair).map_err(io)?;
            }
            if let Some(chain) = &out.linear {
                write_linear_output(&out_prefix, &chain.total).map_err(io)?;
            }
            let write_s = t.elapsed().as_secs_f64();
            Ok(Run {
                recipe,
                adapter: adapter_name(&g),
                grid: out.grid.size(),
                total_s: t_all.elapsed().as_secs_f64(),
                write_s,
                timings: out.timings,
                levels: out
                    .levels
                    .iter()
                    .map(|l| {
                        let e = l.iterations.last().map(|i| i.energy).unwrap_or(0.0);
                        (l.level, l.grid_size, l.iterations.len(), l.converged, e)
                    })
                    .collect(),
                affine_parameters: out.linear.as_ref().map(|c| c.total.parameters()),
                affine_center: out.linear.as_ref().map(|c| c.total.center),
                peak_gpu_bytes: g.peak_allocated_bytes(),
                memory_budget_bytes: caps.memory_budget,
            })
        })
    })?;

    let d = PyDict::new(py);
    d.set_item("adapter", run.adapter)?;
    d.set_item("grid", run.grid)?;
    d.set_item("total_s", run.total_s)?;
    d.set_item("resample_s", run.timings.resample_s)?;
    d.set_item("preprocess_s", run.timings.preprocess_s)?;
    d.set_item("linear_s", run.timings.linear_s)?;
    d.set_item("syn_s", run.timings.syn_s)?;
    d.set_item("warp_s", run.timings.warp_s)?;
    d.set_item("write_s", run.write_s)?;
    d.set_item("levels", run.levels)?;
    d.set_item("affine_parameters", run.affine_parameters)?;
    d.set_item("affine_center", run.affine_center)?;
    d.set_item("peak_gpu_bytes", run.peak_gpu_bytes)?;
    d.set_item("memory_budget_bytes", run.memory_budget_bytes)?;
    d.set_item("recipe", run.recipe.to_dict(py)?)?;
    Ok(d)
}

/// Opens the GPU the engine would use and describes it; raises `GpuUnavailableError` when there
/// is none.
#[pyfunction]
#[pyo3(signature = (adapter=None))]
fn gpu_info<'py>(py: Python<'py>, adapter: Option<String>) -> PyResult<Bound<'py, PyDict>> {
    let (name, backend, device_type, driver, budget) = py.detach(move || {
        open_gpu(adapter).map(|g| {
            let i = &g.context().info;
            let caps = CapacityCaps::from_context(g.context());
            (
                i.name.clone(),
                i.backend.clone(),
                i.device_type.clone(),
                i.driver.clone(),
                caps.memory_budget,
            )
        })
    })?;
    let d = PyDict::new(py);
    d.set_item("name", name)?;
    d.set_item("backend", backend)?;
    d.set_item("device_type", device_type)?;
    d.set_item("driver", driver)?;
    d.set_item("memory_budget_bytes", budget)?;
    Ok(d)
}

/// Whether a registration grid of `dims` voxels and `channels` channels fits the GPU.
#[pyfunction]
#[pyo3(signature = (dims, channels, adapter=None))]
fn fits_gpu(
    py: Python<'_>,
    dims: [usize; 3],
    channels: usize,
    adapter: Option<String>,
) -> PyResult<bool> {
    py.detach(move || {
        let g = open_gpu(adapter)?;
        Ok(plan_capacity(dims, channels, &CapacityCaps::from_context(g.context())).is_ok())
    })
}

/// A reference grid from Python: size, spacing, origin, direction (ITK / LPS).
type GridArgs = ([usize; 3], Vec3, Vec3, Mat3);

fn to_grid(g: &GridArgs) -> PyResult<Grid> {
    Grid::new(g.0, g.1, g.2, g.3)
        .map_err(|e| PyValueError::new_err(format!("bad reference geometry: {e}")))
}

/// The chain `ants.apply_transforms(transformlist=transforms)` applies: a reference point goes
/// through `transforms[0]` first (measured against ANTsPy, see `tests/test_post.py`). The queue
/// applies its last element first, so the files are pushed in reverse.
fn read_chain(transforms: &[PathBuf]) -> PyResult<TransformChain> {
    if transforms.is_empty() {
        return Err(PyValueError::new_err("give at least one transform file"));
    }
    let mut chain = TransformChain::new();
    for path in transforms.iter().rev() {
        let part = read_transform_file(path)
            .map_err(|e| RegistrationError::new_err(format!("reading {}: {e}", path.display())))?;
        chain.extend(part);
    }
    Ok(chain)
}

/// Writes the transforms as one displacement field on `reference` (`antsApplyTransforms
/// --compose`: `float32` vectors, intent `NIFTI_INTENT_VECTOR`, parallel gzip for `.nii.gz` at
/// `compression_level`, 0 to 9).
#[pyfunction]
#[pyo3(signature = (transforms, reference, out_path, compression_level=None))]
fn compose_field(
    py: Python<'_>,
    transforms: Vec<PathBuf>,
    reference: GridArgs,
    out_path: PathBuf,
    compression_level: Option<u32>,
) -> PyResult<()> {
    if compression_level.is_some_and(|l| l > 9) {
        return Err(PyValueError::new_err("compression_level must be 0 to 9"));
    }
    let grid = to_grid(&reference)?;
    py.detach(move || {
        guarded(move || {
            let chain = read_chain(&transforms)?;
            let field = post::compose_field(&chain, &grid).map_err(reg_err)?;
            post::write_field(&out_path, &field, compression_level).map_err(|e| {
                RegistrationError::new_err(format!("writing {}: {e}", out_path.display()))
            })
        })
    })
}

/// Resamples `moving` onto `reference` through the transforms (`antsApplyTransforms`), with the
/// ANTs interpolator `linear`, `nearestNeighbor` or `lanczosWindowedSinc`. Returns the voxels as
/// a flat `float32` array, x fastest.
#[pyfunction]
#[pyo3(signature = (moving, reference, transforms, interpolation="linear", default_value=0.0))]
fn apply_transforms<'py>(
    py: Python<'py>,
    moving: ImageArgs<'py>,
    reference: GridArgs,
    transforms: Vec<PathBuf>,
    interpolation: &str,
    default_value: f64,
) -> PyResult<Bound<'py, PyArray1<f32>>> {
    let interp = post::Interp::from_ants_name(interpolation).ok_or_else(|| {
        PyValueError::new_err(format!(
            "interpolation {interpolation:?} is not supported (linear, nearestNeighbor, \
             lanczosWindowedSinc)"
        ))
    })?;
    let grid = to_grid(&reference)?;
    let image = to_volume(&moving)?;
    let out = py.detach(move || {
        guarded(move || {
            let chain = read_chain(&transforms)?;
            post::resample(&image, &grid, &chain, interp, default_value).map_err(reg_err)
        })
    })?;
    Ok(PyArray1::from_vec(py, out))
}

/// Linear fits of one image pair on the GPU (one `Rigid` or `Affine` stage on Mattes MI with
/// regular sampling per fit). The images are converted, winsorized (`(lower, upper)` quantiles,
/// rescaled to [0, 1] as ANTs does) and uploaded once; smoothed copies are kept between fits, so
/// a multi-start, or a rigid fit followed by an affine one, pays the preparation once.
/// `fixed_mask`: its non-zero voxels are where the metric samples, in fits that ask for it (a fit
/// can bring its own mask instead). Holds the GPU until `close()` (or the `with` block ends).
#[pyclass(module = "fastwarp", frozen)]
struct LinearSession {
    inner: Mutex<Option<session::Session>>,
    com: AffineTransform,
    adapter: String,
}

fn closed() -> PyErr {
    PyValueError::new_err("the session is closed")
}

#[pymethods]
impl LinearSession {
    #[new]
    #[pyo3(signature = (fixed, moving, fixed_mask=None, winsorize=None, adapter=None))]
    fn new(
        py: Python<'_>,
        fixed: ImageArgs<'_>,
        moving: ImageArgs<'_>,
        fixed_mask: Option<ImageArgs<'_>>,
        winsorize: Option<(f32, f32)>,
        adapter: Option<String>,
    ) -> PyResult<Self> {
        if let Some((lo, hi)) = winsorize {
            if !(0.0 <= lo && lo < hi && hi <= 1.0) {
                return Err(PyValueError::new_err(
                    "winsorize must be (lower, upper) quantiles with 0 <= lower < upper <= 1",
                ));
            }
        }
        let fixed = to_volume(&fixed)?;
        let moving = to_volume(&moving)?;
        let mask = fixed_mask
            .map(|m| to_volume(&m).map(MaskVolume::from_volume))
            .transpose()?;
        let (session, com, name) = py.detach(move || {
            guarded(move || {
                let gpu = open_gpu(adapter)?;
                // ANTs initialises on the images as given, before winsorizing.
                let com = initial_transform(&fixed, &moving, InitFeature::CentreOfMass)
                    .map_err(reg_err)?;
                let (fixed, moving) = match winsorize {
                    Some((lo, hi)) => (
                        preprocess_volume(&fixed, lo, hi, 0.0, 1.0).map_err(reg_err)?,
                        preprocess_volume(&moving, lo, hi, 0.0, 1.0).map_err(reg_err)?,
                    ),
                    None => (fixed, moving),
                };
                let name = adapter_name(&gpu);
                let session = session::Session {
                    gpu,
                    fixed,
                    moving,
                    mask,
                    cache: Default::default(),
                };
                Ok((session, com, name))
            })
        })?;
        Ok(Self {
            inner: Mutex::new(Some(session)),
            com,
            adapter: name,
        })
    }

    /// The GPU in use.
    #[getter]
    fn adapter(&self) -> String {
        self.adapter.clone()
    }

    /// The centre-of-mass start (`parameters`, `center`), what `initial=None` uses.
    #[getter]
    fn com_initial(&self) -> ([f64; 12], [f64; 3]) {
        (self.com.parameters(), self.com.center)
    }

    /// One fit from `initial` (`(parameters, center)` of an ITK affine mapping fixed points to
    /// moving ones; `None` = centres of intensity mass), with `stage`'s schedule (the fields of
    /// `cohort_recipe()['rigid']`) and `transform` `"rigid"` or `"affine"`. With `use_mask` the
    /// metric samples only inside `mask` (an image on any grid; `None` = the session's
    /// `fixed_mask`). Writes `<out_prefix>0GenericAffine.mat` when given. Returns the collapsed
    /// affine (`parameters`, `center`), the final MI value (`metric`, lower is better; `None` when
    /// the last level ran no iteration), the start, iterations per level and the time. The GIL is
    /// released while it runs.
    #[pyo3(signature = (
        stage=None, initial=None, use_mask=true, out_prefix=None, cancel=None,
        transform="rigid", mask=None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn register<'py>(
        &self,
        py: Python<'py>,
        stage: Option<Bound<'py, PyDict>>,
        initial: Option<([f64; 12], [f64; 3])>,
        use_mask: bool,
        out_prefix: Option<PathBuf>,
        cancel: Option<PyRef<'py, CancelToken>>,
        transform: &str,
        mask: Option<ImageArgs<'py>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let recipe = recipe::linear_stage(stage.as_ref(), transform)?;
        let stage_out = recipe::linear_stage_dict(py, &recipe)?;
        if mask.is_some() && !use_mask {
            return Err(PyValueError::new_err(
                "a mask was given with use_mask=False; pass one or the other",
            ));
        }
        let call_mask = mask
            .map(|m| to_volume(&m).map(MaskVolume::from_volume))
            .transpose()?;
        let start = match initial {
            Some((params, center)) => AffineTransform::from_parameters(&params, &center)
                .map_err(|e| PyValueError::new_err(format!("bad initial transform: {e}")))?,
            None => self.com,
        };
        if let Some(dir) = out_prefix.as_ref().and_then(|p| p.parent()) {
            if !dir.as_os_str().is_empty() && !dir.is_dir() {
                return Err(PyValueError::new_err(format!(
                    "output folder {} does not exist",
                    dir.display()
                )));
            }
        }
        let flag = cancel.map(|c| Arc::clone(&c.flag));
        // Taken out for the fit (it runs without the GIL) and put back after, so a second
        // thread using the same session meanwhile is told so instead of waiting on a lock.
        let mut session = self
            .inner
            .lock()
            .map_err(|_| closed())?
            .take()
            .ok_or_else(|| PyValueError::new_err("the session is closed or busy"))?;
        let t = Instant::now();
        let (session, fit) = py.detach(move || {
            let fit = guarded(|| {
                let mask = match (use_mask, call_mask) {
                    (false, _) => None,
                    (true, Some(m)) => Some(m),
                    (true, None) => session.mask.clone(),
                };
                session
                    .fit(&recipe, start, mask, flag.as_deref())
                    .map_err(reg_err)
            });
            (session, fit)
        });
        *self.inner.lock().map_err(|_| closed())? = Some(session);
        let fit = fit?;
        if let Some(prefix) = &out_prefix {
            write_linear_output(prefix, &fit.total)
                .map_err(|e| RegistrationError::new_err(format!("writing outputs: {e}")))?;
        }
        let d = PyDict::new(py);
        d.set_item("adapter", &self.adapter)?;
        d.set_item("parameters", fit.total.parameters())?;
        d.set_item("center", fit.total.center)?;
        d.set_item("initial_parameters", start.parameters())?;
        d.set_item("initial_center", start.center)?;
        d.set_item("metric", fit.metric)?;
        d.set_item("iterations", fit.iterations)?;
        d.set_item("total_s", t.elapsed().as_secs_f64())?;
        d.set_item("transform", transform)?;
        d.set_item("stage", stage_out)?;
        Ok(d)
    }

    /// Releases the GPU and the kept images.
    fn close(&self) -> PyResult<()> {
        *self.inner.lock().map_err(|_| closed())? = None;
        Ok(())
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __exit__(
        &self,
        _exc_type: Option<Bound<'_, PyAny>>,
        _exc: Option<Bound<'_, PyAny>>,
        _tb: Option<Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        self.close()?;
        Ok(false)
    }
}

/// `stage` checked as `LinearSession.register` checks it for `transform` (unknown keys, types,
/// level counts, the GPU's 32 MI bins), returned complete; raises `ValueError` without touching
/// the GPU.
#[pyfunction]
#[pyo3(signature = (stage=None, transform="rigid"))]
fn check_linear_stage<'py>(
    py: Python<'py>,
    stage: Option<Bound<'py, PyDict>>,
    transform: &str,
) -> PyResult<Bound<'py, PyDict>> {
    recipe::linear_stage_dict(py, &recipe::linear_stage(stage.as_ref(), transform)?)
}

/// The dbs-localizer cohort recipe for 1 or 2 channels, in the form `register_syn(recipe=...)`
/// takes; every field present.
#[pyfunction]
#[pyo3(signature = (channels=1))]
fn cohort_recipe(py: Python<'_>, channels: usize) -> PyResult<Bound<'_, PyDict>> {
    if !(1..=2).contains(&channels) {
        return Err(PyValueError::new_err("channels must be 1 or 2"));
    }
    recipe::Recipe::cohort(channels).to_dict(py)
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(register_syn, m)?)?;
    m.add_function(wrap_pyfunction!(gpu_info, m)?)?;
    m.add_function(wrap_pyfunction!(fits_gpu, m)?)?;
    m.add_function(wrap_pyfunction!(compose_field, m)?)?;
    m.add_function(wrap_pyfunction!(apply_transforms, m)?)?;
    m.add_function(wrap_pyfunction!(cohort_recipe, m)?)?;
    m.add_class::<LinearSession>()?;
    m.add_function(wrap_pyfunction!(check_linear_stage, m)?)?;
    m.add_class::<CancelToken>()?;
    m.add("GpuUnavailableError", py.get_type::<GpuUnavailableError>())?;
    m.add("GpuCapacityError", py.get_type::<GpuCapacityError>())?;
    m.add("RegistrationError", py.get_type::<RegistrationError>())?;
    m.add(
        "RegistrationCancelled",
        py.get_type::<RegistrationCancelled>(),
    )?;
    m.add("ENGINE_SOURCE", "fastwarp-legacy 90c1df3")?;
    m.add("VERSION", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
