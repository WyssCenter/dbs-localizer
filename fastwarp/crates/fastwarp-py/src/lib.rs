// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! Python extension `fastwarp._core`: registration on the GPU (by default the dbs-localizer
//! cohort chain: centre of mass, Rigid, Affine, SyN; any start, linear stages and optional SyN
//! stage with a stage-list recipe), with ANTs-compatible transform files.
//!
//! Images arrive as flat `float32` or `float64` arrays in ITK voxel order (x fastest) with ITK
//! (LPS) geometry, i.e. exactly what an `ANTsImage` holds; the Python wrapper does the flattening
//! (a view, no copy, for an `ANTsImage`). They are converted to the engine's `f64` once, here.
//! The engine calls themselves are those of the `fastwarp` command line (`fastwarp_cli::engine`),
//! which `cli_main` runs.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod ants;
mod recipe;

use fastwarp_cli::engine::{self, Failure, SynOutputs};
use fastwarp_cli::session;
use fastwarp_reg::mask::MaskVolume;
use fastwarp_reg::register::write_linear_output;
use fastwarp_reg::transform::AffineTransform;
use fastwarp_reg::{Grid, Mat3, Vec3, Volume};
use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

create_exception!(
    fastwarp,
    GpuUnavailableError,
    PyRuntimeError,
    "No usable GPU; raised before any work, so a caller can run ANTs instead (software adapters count only with FASTWARP_GPU_ALLOW_SOFTWARE=1)."
);
create_exception!(
    fastwarp,
    GpuCapacityError,
    PyRuntimeError,
    "The registration does not fit the GPU's memory budget or buffer limit; raised by register_syn before any work."
);
create_exception!(
    fastwarp,
    RegistrationError,
    PyRuntimeError,
    "The engine failed while it worked (for example NaN voxels, GPU out of memory, or a file that cannot be read or written)."
);
create_exception!(
    fastwarp,
    RegistrationCancelled,
    PyException,
    "The run was stopped through its CancelToken; no files are written."
);
create_exception!(
    fastwarp,
    UnsupportedArgument,
    PyValueError,
    "An argument of fastwarp.ants that would change the result and that FastWarp does not support; raised before any work. A ValueError."
);

/// An engine failure as the Python exception the API documents.
fn py_err(f: Failure) -> PyErr {
    let msg = f.to_string();
    match f {
        Failure::Invalid(_) => PyValueError::new_err(msg),
        Failure::GpuUnavailable(_) => GpuUnavailableError::new_err(msg),
        Failure::GpuCapacity(_) => GpuCapacityError::new_err(msg),
        Failure::Cancelled => RegistrationCancelled::new_err(msg),
        Failure::Registration(_) | Failure::Read(_) | Failure::Write(_) => {
            RegistrationError::new_err(msg)
        }
    }
}

/// A flag that stops a running registration or fit from another thread: pass it as `cancel=`,
/// then call `cancel()`; the run stops at its next iteration and raises `RegistrationCancelled`.
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

    /// Ask the run that uses this token to stop at its next iteration.
    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether `cancel()` has been called.
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

/// Registers `moving` to `fixed` (1 or 2 channels each) on the GPU with `recipe` (default: the
/// dbs-localizer cohort chain) and writes the transforms under `out_prefix` in the `outputs`
/// layout (`"composite"`: `Composite.h5`, `InverseComposite.h5`, `0GenericAffine.mat`;
/// `"split"`: `0GenericAffine.mat` and, after a SyN stage, `1Warp.nii.gz` / `1InverseWarp.nii.gz`;
/// `"both"`), and with `write_warped` `<out_prefix>_Warped.nii.gz`. Returns a dict of timings and
/// diagnostics. The GIL is released while it runs.
#[pyfunction]
#[pyo3(signature = (fixed, moving, out_prefix, iso_mm=1.0, write_warped=false, adapter=None, cancel=None, recipe=None, outputs="composite"))]
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
    outputs: &str,
) -> PyResult<Bound<'py, PyDict>> {
    engine::check_channels(fixed.len(), moving.len()).map_err(py_err)?;
    // Checked before any work, like everything a caller can get wrong.
    let recipe = recipe::from_dict(recipe.as_ref(), fixed.len())?;
    let layout = engine::output_layout(outputs).map_err(py_err)?;
    engine::check_iso_mm(iso_mm).map_err(py_err)?;
    // Checked up front: otherwise a missing folder only shows when the outputs are written.
    engine::check_out_folder(&out_prefix).map_err(py_err)?;
    let fixed: Vec<Volume<f64>> = fixed.iter().map(to_volume).collect::<PyResult<_>>()?;
    let moving: Vec<Volume<f64>> = moving.iter().map(to_volume).collect::<PyResult<_>>()?;
    let flag = cancel.map(|c| Arc::clone(&c.flag));
    let outputs = SynOutputs {
        prefix: out_prefix,
        write_warped,
        layout,
    };

    let run = py
        .detach(move || {
            engine::register_syn(fixed, moving, Some(outputs), iso_mm, adapter, flag, recipe)
        })
        .map_err(py_err)?;

    let d = PyDict::new(py);
    d.set_item("adapter", &run.adapter)?;
    d.set_item("grid", run.grid)?;
    d.set_item("total_s", run.total_s)?;
    d.set_item("resample_s", run.timings.resample_s)?;
    d.set_item("preprocess_s", run.timings.preprocess_s)?;
    d.set_item("linear_s", run.timings.linear_s)?;
    d.set_item("syn_s", run.timings.syn_s)?;
    d.set_item("warp_s", run.timings.warp_s)?;
    d.set_item("write_s", run.write_s)?;
    d.set_item(
        "levels",
        run.levels
            .iter()
            .map(|l| (l.level, l.grid_size, l.iterations, l.converged, l.energy))
            .collect::<Vec<_>>(),
    )?;
    d.set_item("affine_parameters", run.affine_parameters)?;
    d.set_item("affine_center", run.affine_center)?;
    d.set_item("initial_parameters", run.initial_parameters)?;
    d.set_item("initial_center", run.initial_center)?;
    let stages = PyList::empty(py);
    for s in &run.linear_stages {
        let e = PyDict::new(py);
        e.set_item("transform", s.transform)?;
        e.set_item("iterations", s.iterations.clone())?;
        e.set_item("metric", s.metric)?;
        stages.append(e)?;
    }
    d.set_item("linear_stages", stages)?;
    d.set_item("deformable", run.deformable)?;
    d.set_item("warnings", run.warnings.clone())?;
    let files: Vec<String> = run
        .files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    d.set_item("files", files)?;
    d.set_item("peak_gpu_bytes", run.peak_gpu_bytes)?;
    d.set_item("memory_budget_bytes", run.memory_budget_bytes)?;
    d.set_item("recipe", recipe::to_dict(py, &run.recipe)?)?;
    Ok(d)
}

/// Open the GPU that FastWarp would use and describe it: `name`, `backend` (graphics API),
/// `device_type`, `driver` (driver name, not a version) and `memory_budget_bytes`. Raises
/// `GpuUnavailableError` when there is no usable GPU.
#[pyfunction]
#[pyo3(signature = (adapter=None))]
fn gpu_info<'py>(py: Python<'py>, adapter: Option<String>) -> PyResult<Bound<'py, PyDict>> {
    let info = py
        .detach(move || engine::gpu_info(adapter))
        .map_err(py_err)?;
    let d = PyDict::new(py);
    d.set_item("name", info.name)?;
    d.set_item("backend", info.backend)?;
    d.set_item("device_type", info.device_type)?;
    d.set_item("driver", info.driver)?;
    d.set_item("memory_budget_bytes", info.memory_budget_bytes)?;
    Ok(d)
}

/// Whether a registration of a `dims` fixed grid and a `moving_dims` moving grid (default: the
/// fixed grid) with `channels` channels and `recipe` (default: the cohort chain) fits the GPU, as
/// `register_syn` checks it.
#[pyfunction]
#[pyo3(signature = (dims, channels, adapter=None, moving_dims=None, recipe=None))]
fn fits_gpu(
    py: Python<'_>,
    dims: [usize; 3],
    channels: usize,
    adapter: Option<String>,
    moving_dims: Option<[usize; 3]>,
    recipe: Option<Bound<'_, PyDict>>,
) -> PyResult<bool> {
    if !(1..=2).contains(&channels) {
        return Err(PyValueError::new_err("channels must be 1 or 2"));
    }
    let recipe = recipe::from_dict(recipe.as_ref(), channels)?;
    py.detach(move || engine::fits(dims, moving_dims, channels, adapter, &recipe))
        .map_err(py_err)
}

/// A reference grid from Python: size, spacing, origin, direction (ITK / LPS).
type GridArgs = ([usize; 3], Vec3, Vec3, Mat3);

fn to_grid(g: &GridArgs) -> PyResult<Grid> {
    Grid::new(g.0, g.1, g.2, g.3)
        .map_err(|e| PyValueError::new_err(format!("bad reference geometry: {e}")))
}

/// Writes the transforms as one displacement field on `reference` (`antsApplyTransforms
/// --compose`: `float32` vectors, intent `NIFTI_INTENT_VECTOR`, parallel gzip for `.nii.gz` at
/// `compression_level`, 0 to 9). A reference point goes through `transforms[0]` first, as in
/// `ants.apply_transforms(transformlist=transforms)`; `invert` (one flag per transform, ANTsPy's
/// `whichtoinvert`) uses the inverse of a linear transform.
#[pyfunction]
#[pyo3(signature = (transforms, reference, out_path, compression_level=None, invert=None))]
fn compose_field(
    py: Python<'_>,
    transforms: Vec<PathBuf>,
    reference: GridArgs,
    out_path: PathBuf,
    compression_level: Option<u32>,
    invert: Option<Vec<bool>>,
) -> PyResult<()> {
    engine::check_compression_level(compression_level).map_err(py_err)?;
    let grid = to_grid(&reference)?;
    py.detach(move || {
        engine::compose(
            &transforms,
            invert.as_deref(),
            &grid,
            &out_path,
            compression_level,
        )
    })
    .map_err(py_err)
}

/// Resamples `moving` onto `reference` through the transforms (`antsApplyTransforms`), with the
/// ANTs interpolator `linear`, `nearestNeighbor` or `lanczosWindowedSinc`; `invert` (one flag
/// per transform, ANTsPy's `whichtoinvert`) uses the inverse of a linear transform. Returns the
/// voxels as a flat `float32` array, x fastest.
#[pyfunction]
#[pyo3(signature = (moving, reference, transforms, interpolation="linear", default_value=0.0, invert=None))]
#[allow(clippy::too_many_arguments)]
fn apply_transforms<'py>(
    py: Python<'py>,
    moving: ImageArgs<'py>,
    reference: GridArgs,
    transforms: Vec<PathBuf>,
    interpolation: &str,
    default_value: f64,
    invert: Option<Vec<bool>>,
) -> PyResult<Bound<'py, PyArray1<f32>>> {
    let interp = engine::interpolator(interpolation).map_err(py_err)?;
    let grid = to_grid(&reference)?;
    let image = to_volume(&moving)?;
    let out = py
        .detach(move || {
            engine::apply(
                &image,
                &grid,
                &transforms,
                invert.as_deref(),
                interp,
                default_value,
            )
        })
        .map_err(py_err)?;
    Ok(PyArray1::from_vec(py, out))
}

/// Linear fits of one image pair on the GPU (one `Rigid` or `Affine` stage on Mattes MI with
/// regular sampling per fit). The images are converted, winsorized (`(lower, upper)` quantiles,
/// rescaled to [0, 1] as ANTs does) and uploaded once; smoothed copies are kept between fits, so
/// a multi-start, or a rigid fit followed by an affine one, pays the preparation once.
/// `fixed_mask`: its non-zero voxels are where the metric samples, in fits that ask for it (a fit
/// can bring its own mask instead). Holds the GPU until `close()` (or the `with` block ends).
#[pyclass(module = "fastwarp._core", frozen)]
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
        engine::check_winsorize(winsorize).map_err(py_err)?;
        let fixed = to_volume(&fixed)?;
        let moving = to_volume(&moving)?;
        let mask = fixed_mask
            .map(|m| to_volume(&m).map(MaskVolume::from_volume))
            .transpose()?;
        let start = py
            .detach(move || engine::open_linear(fixed, moving, mask, winsorize, adapter))
            .map_err(py_err)?;
        Ok(Self {
            inner: Mutex::new(Some(start.session)),
            com: start.com,
            adapter: start.adapter,
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
        if let Some(prefix) = &out_prefix {
            engine::check_out_folder(prefix).map_err(py_err)?;
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
            let fit = engine::guarded(|| {
                let mask = match (use_mask, call_mask) {
                    (false, _) => None,
                    (true, Some(m)) => Some(m),
                    (true, None) => session.mask.clone(),
                };
                session
                    .fit(&recipe, start, mask, flag.as_deref())
                    .map_err(engine::reg_err)
            });
            (session, fit)
        });
        *self.inner.lock().map_err(|_| closed())? = Some(session);
        let fit = fit.map_err(py_err)?;
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
        d.set_item("warnings", fit.warnings)?;
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
    recipe::to_dict(py, &fastwarp_cli::recipe::Recipe::cohort(channels))
}

/// Runs the `fastwarp` command line on `argv` (the arguments after the program name) and returns
/// its exit status; output goes to the process's standard output and error. The GIL is released
/// while it runs.
#[pyfunction]
fn cli_main(py: Python<'_>, argv: Vec<OsString>) -> i32 {
    py.detach(move || {
        let mut args = vec![OsString::from("fastwarp")];
        args.extend(argv);
        fastwarp_cli::run(args, &mut std::io::stdout(), &mut std::io::stderr())
    })
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
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    m.add_function(wrap_pyfunction!(ants::ants_parse_registration, m)?)?;
    m.add_function(wrap_pyfunction!(ants::ants_parse_apply, m)?)?;
    m.add_function(wrap_pyfunction!(ants::ants_resolve_seed, m)?)?;
    m.add_function(wrap_pyfunction!(ants::ants_registration, m)?)?;
    m.add_function(wrap_pyfunction!(ants::ants_apply_transforms, m)?)?;
    m.add("UnsupportedArgument", py.get_type::<UnsupportedArgument>())?;
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
