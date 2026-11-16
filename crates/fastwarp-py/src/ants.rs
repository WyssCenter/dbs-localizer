// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The ANTs compatibility mode for `fastwarp.ants`: the parsers and runners of
//! `fastwarp ants-registration` and `fastwarp ants-apply-transforms` (`fastwarp_cli::ants`), with
//! the images of an ANTsPy call handed over in memory under the names the argument list gives
//! them, and the warped images handed back the same way. A call therefore runs exactly what the
//! subcommand runs on the same argument list.

use std::path::PathBuf;
use std::sync::Arc;

use fastwarp_cli::ants::{
    parse_apply, parse_registration, resolve_seed, run_apply_plan, run_registration, ApplyCall,
    Images, KeptImage, MemoryImage, Refusal, RegistrationCall, RunOptions,
};
use fastwarp_cli::{CliError, ExitStatus};
use numpy::PyArray1;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::{
    recipe, to_grid, to_volume, CancelToken, GpuCapacityError, GpuUnavailableError, GridArgs,
    ImageArgs, RegistrationCancelled, RegistrationError, UnsupportedArgument,
};

const REGISTRATION: &str = "fastwarp.ants.registration";
const APPLY: &str = "fastwarp.ants.apply_transforms";

/// A usage error: `UnsupportedArgument` for something FastWarp does not support, `ValueError`
/// for an invalid argument.
fn usage(program: &str, message: &str, unsupported: bool) -> PyErr {
    let text = format!("{program}: {message}");
    if unsupported {
        UnsupportedArgument::new_err(text)
    } else {
        PyValueError::new_err(text)
    }
}

fn refused(program: &str, r: Refusal) -> PyErr {
    usage(program, &r.0, r.is_unsupported())
}

fn failed(program: &str, e: CliError) -> PyErr {
    match e.status {
        ExitStatus::Usage => usage(program, &e.message, e.unsupported),
        ExitStatus::GpuUnavailable => GpuUnavailableError::new_err(e.message),
        ExitStatus::GpuCapacity => GpuCapacityError::new_err(e.message),
        ExitStatus::Cancelled => RegistrationCancelled::new_err(e.message),
        ExitStatus::Ok | ExitStatus::Registration | ExitStatus::Io => {
            RegistrationError::new_err(format!("{program}: {}", e.message))
        }
    }
}

fn texts(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// The kept images as `{name: (flat float32 voxels, size, spacing, origin, direction)}`.
fn kept_dict(py: Python<'_>, kept: Vec<KeptImage>) -> PyResult<Bound<'_, PyDict>> {
    let d = PyDict::new(py);
    for k in kept {
        let g = k.grid;
        d.set_item(
            k.name.to_string_lossy().into_owned(),
            (
                PyArray1::from_vec(py, k.data),
                g.size(),
                g.spacing(),
                g.origin(),
                g.direction(),
            ),
        )?;
    }
    Ok(d)
}

fn text_result<'py>(py: Python<'py>, kind: &str, text: String) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("kind", kind)?;
    d.set_item("text", text)?;
    Ok(d)
}

fn memory_images(
    images: &[(PathBuf, ImageArgs<'_>)],
    grids: &[(PathBuf, GridArgs)],
) -> PyResult<Vec<(PathBuf, MemoryImage)>> {
    let mut out = Vec::with_capacity(images.len() + grids.len());
    for (name, img) in images {
        out.push((name.clone(), MemoryImage::Volume(to_volume(img)?)));
    }
    for (name, grid) in grids {
        out.push((name.clone(), MemoryImage::Grid(to_grid(grid)?)));
    }
    Ok(out)
}

/// Parses an `antsRegistration` argument list (without the program name) as
/// `fastwarp ants-registration` does, without reading a file: `{"kind": "run", "plan": ...}`
/// with the plan as JSON, or `{"kind": "help" | "version", "text": ...}`. A refused argument
/// raises `UnsupportedArgument`, an invalid one `ValueError`.
#[pyfunction]
pub fn ants_parse_registration<'py>(
    py: Python<'py>,
    argv: Vec<String>,
) -> PyResult<Bound<'py, PyDict>> {
    match parse_registration(&argv).map_err(|r| refused(REGISTRATION, r))? {
        RegistrationCall::Help(t) => text_result(py, "help", t),
        RegistrationCall::Version(t) => text_result(py, "version", t),
        RegistrationCall::Run(p) => {
            let d = PyDict::new(py);
            d.set_item("kind", "run")?;
            d.set_item("plan", recipe::from_json(py, &p.to_json())?)?;
            Ok(d)
        }
    }
}

/// Parses an `antsApplyTransforms` argument list as `fastwarp ants-apply-transforms` does,
/// without reading a file; the result and errors as for `ants_parse_registration`.
#[pyfunction]
pub fn ants_parse_apply<'py>(py: Python<'py>, argv: Vec<String>) -> PyResult<Bound<'py, PyDict>> {
    match parse_apply(&argv).map_err(|r| refused(APPLY, r))? {
        ApplyCall::Help(t) => text_result(py, "help", t),
        ApplyCall::Version(t) => text_result(py, "version", t),
        ApplyCall::Run(p) => {
            let d = PyDict::new(py);
            d.set_item("kind", "run")?;
            d.set_item("plan", recipe::from_json(py, &p.to_json())?)?;
            Ok(d)
        }
    }
}

/// The seed of every linear stage: `random_seed` (`--random-seed`) when given, else
/// `ants_random_seed` (the text of `ANTS_RANDOM_SEED`) when given, else FastWarp's default.
#[pyfunction]
#[pyo3(signature = (random_seed=None, ants_random_seed=None))]
pub fn ants_resolve_seed(
    random_seed: Option<u32>,
    ants_random_seed: Option<&str>,
) -> PyResult<u32> {
    resolve_seed(random_seed, ants_random_seed).map_err(|r| refused(REGISTRATION, r))
}

/// Runs an `antsRegistration` argument list as `fastwarp ants-registration` does. `images` maps
/// names in the argument list to images held in memory (each is used once); the output images
/// named in `keep` are returned instead of written. `iso_mm` overrides the registration grid's
/// voxel size (default: the fixed image's smallest spacing). Returns `{"kind": "run", "plan",
/// "warnings", "transform_files", "files", "kept", "report", "iso_mm", "grid", "adapter",
/// "total_s"}`, or the help or version text. The GIL is released while it runs.
#[pyfunction]
#[pyo3(signature = (argv, images=Vec::new(), keep=Vec::new(), iso_mm=None, adapter=None, cancel=None))]
#[allow(clippy::too_many_arguments)]
pub fn ants_registration<'py>(
    py: Python<'py>,
    argv: Vec<String>,
    images: Vec<(PathBuf, ImageArgs<'py>)>,
    keep: Vec<PathBuf>,
    iso_mm: Option<f64>,
    adapter: Option<String>,
    cancel: Option<PyRef<'py, CancelToken>>,
) -> PyResult<Bound<'py, PyDict>> {
    let plan = match parse_registration(&argv).map_err(|r| refused(REGISTRATION, r))? {
        RegistrationCall::Help(t) => return text_result(py, "help", t),
        RegistrationCall::Version(t) => return text_result(py, "version", t),
        RegistrationCall::Run(p) => *p,
    };
    let inputs = memory_images(&images, &[])?;
    drop(images);
    let seed = std::env::var_os("ANTS_RANDOM_SEED").map(|v| v.to_string_lossy().into_owned());
    let mut warnings = plan.warnings.clone();
    let transform_files = plan.transform_files();
    let plan_json = plan.to_json();
    let options = RunOptions {
        iso_mm,
        adapter,
        cancel: cancel.map(|c| Arc::clone(&c.flag)),
    };
    let done = py
        .detach(move || run_registration(plan, seed.as_deref(), Images::new(inputs, keep), options))
        .map_err(|e| failed(REGISTRATION, e))?;
    warnings.extend(done.run.warnings.iter().cloned());
    let d = PyDict::new(py);
    d.set_item("kind", "run")?;
    d.set_item("plan", recipe::from_json(py, &plan_json)?)?;
    d.set_item("warnings", warnings)?;
    d.set_item("transform_files", texts(&transform_files))?;
    d.set_item("files", texts(&done.run.files))?;
    d.set_item("kept", kept_dict(py, done.kept)?)?;
    d.set_item("report", done.report)?;
    d.set_item("iso_mm", done.iso_mm)?;
    d.set_item("grid", done.run.grid)?;
    d.set_item("adapter", done.run.adapter)?;
    d.set_item("total_s", done.run.total_s)?;
    Ok(d)
}

/// Runs an `antsApplyTransforms` argument list as `fastwarp ants-apply-transforms` does, on the
/// CPU. `images` and `grids` map names in the argument list to images and reference grids held
/// in memory; an output image named in `keep` is returned instead of written. Returns
/// `{"kind": "run", "plan", "warnings", "kept"}`, or the help or version text. The GIL is
/// released while it runs.
#[pyfunction]
#[pyo3(signature = (argv, images=Vec::new(), grids=Vec::new(), keep=Vec::new()))]
pub fn ants_apply_transforms<'py>(
    py: Python<'py>,
    argv: Vec<String>,
    images: Vec<(PathBuf, ImageArgs<'py>)>,
    grids: Vec<(PathBuf, GridArgs)>,
    keep: Vec<PathBuf>,
) -> PyResult<Bound<'py, PyDict>> {
    let plan = match parse_apply(&argv).map_err(|r| refused(APPLY, r))? {
        ApplyCall::Help(t) => return text_result(py, "help", t),
        ApplyCall::Version(t) => return text_result(py, "version", t),
        ApplyCall::Run(p) => *p,
    };
    let inputs = memory_images(&images, &grids)?;
    drop(images);
    let plan_json = plan.to_json();
    let warnings = plan.warnings.clone();
    let kept = py
        .detach(move || run_apply_plan(&plan, Images::new(inputs, keep)))
        .map_err(|e| failed(APPLY, e))?;
    let d = PyDict::new(py);
    d.set_item("kind", "run")?;
    d.set_item("plan", recipe::from_json(py, &plan_json)?)?;
    d.set_item("warnings", warnings)?;
    d.set_item("kept", kept_dict(py, kept)?)?;
    Ok(d)
}
