// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! PyO3 entry points (`feature = "pyo3"`), installed as the `dbsloc_reconstruct` module.
#![allow(clippy::useless_conversion)]

use std::path::{Path, PathBuf};

use pyo3::create_exception;
use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use crate::ants_argv;
use crate::bids::{resolve_subject_with, BidsError};
use crate::ct_recon::{reconstruct_ct, CtMethod, CtReconRequest};
use crate::options::RunOptions;

create_exception!(dbsloc_reconstruct, PyBidsError, PyValueError);

fn value_err(e: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn bids_err(e: BidsError) -> PyErr {
    let err = PyBidsError::new_err(e.message);
    Python::attach(|py| {
        let _ = err.value(py).setattr("code", e.code);
    });
    err
}

fn parse_options(options_json: &str) -> PyResult<RunOptions> {
    serde_json::from_str(options_json)
        .map_err(|e| PyValueError::new_err(format!("invalid RunOptions JSON: {e}")))
}

#[pyfunction(name = "reconstruct_ct")]
#[pyo3(signature = (subj_id, *, method = "auto", ct = None, template = None, norm_inverse_composite = None, el_model = None, el_model_error = None, metal_dark = true, distal_bloom_mm = None, ants_apply_transforms = None, ants_threads = None, pacer_import = None))]
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_ct_py(
    py: Python<'_>,
    subj_id: &str,
    method: &str,
    ct: Option<PathBuf>,
    template: Option<PathBuf>,
    norm_inverse_composite: Option<PathBuf>,
    el_model: Option<String>,
    el_model_error: Option<String>,
    metal_dark: bool,
    distal_bloom_mm: Option<f64>,
    ants_apply_transforms: Option<PathBuf>,
    ants_threads: Option<i64>,
    pacer_import: Option<PathBuf>,
) -> PyResult<String> {
    let m = CtMethod::parse(method).ok_or_else(|| {
        PyValueError::new_err(format!(
            "E_RECON: unknown recon_method {method:?} for a post-op CT \
             (auto, trac_core, trac_core_native, pacer)"
        ))
    })?;
    let req = CtReconRequest {
        ct,
        template,
        norm_inverse_composite,
        el_model,
        el_model_error,
        metal_dark,
        distal_bloom_mm,
        ants_apply: ants_apply_transforms,
        ants_threads: ants_threads.map(|n| n.clamp(1, i64::from(u32::MAX)) as u32),
        pacer_import,
        ..CtReconRequest::new(subj_id, m)
    };
    let result = py.detach(|| reconstruct_ct(&req, None, &mut |_| {}));
    match result {
        Ok(v) => serde_json::to_string(&v).map_err(value_err),
        Err(e) if e.is_os_error() => Err(PyOSError::new_err(e.to_string())),
        Err(e) => Err(PyValueError::new_err(e.to_string())),
    }
}

/// Creates a GPU device, runs the known-answer self-test and returns what was found:
/// `available`, `adapter`, `backend`, `device_type`, `driver`, `verdict`, `detail`,
/// `self_test_attempts` and `software`.
#[pyfunction]
pub fn gpu_probe(py: Python<'_>) -> PyResult<Py<PyAny>> {
    let probe = py.detach(crate::reg_backend::probe_gpu);
    let d = PyDict::new(py);
    d.set_item("available", probe.available)?;
    d.set_item("adapter", probe.adapter)?;
    d.set_item("backend", probe.backend)?;
    d.set_item("device_type", probe.device_type)?;
    d.set_item("driver", probe.driver)?;
    d.set_item("verdict", probe.verdict)?;
    d.set_item("detail", probe.detail)?;
    d.set_item("self_test_attempts", probe.self_test_attempts)?;
    d.set_item("software", probe.software)?;
    Ok(d.into_any().unbind())
}

#[pyfunction]
pub fn plan_run(options_json: &str) -> PyResult<String> {
    let opts = parse_options(options_json)?;
    let plan = crate::plan_run(&opts).map_err(bids_err)?;
    serde_json::to_string(&plan.to_json(&opts)).map_err(value_err)
}

#[pyfunction]
#[pyo3(signature = (dataset_root, subj_id, *, use_index = true))]
pub fn resolve_subject(dataset_root: PathBuf, subj_id: &str, use_index: bool) -> PyResult<String> {
    let sub = resolve_subject_with(&dataset_root, subj_id, use_index).map_err(bids_err)?;
    serde_json::to_string(&sub.to_json()).map_err(value_err)
}

fn path(s: &str) -> &Path {
    Path::new(s)
}

#[pyfunction]
#[pyo3(signature = (moving, fixed, out_prefix, *, init = None, fixed_mask = None, extra = Vec::new(), binary = "antsRegistration"))]
pub fn ants_coreg_argv(
    moving: &str,
    fixed: &str,
    out_prefix: &str,
    init: Option<&str>,
    fixed_mask: Option<&str>,
    extra: Vec<String>,
    binary: &str,
) -> Vec<String> {
    ants_argv::build_ants_ct_coreg_argv(
        path(binary),
        path(moving),
        path(fixed),
        path(out_prefix),
        init.map(path),
        fixed_mask.map(path),
        &extra,
    )
}

#[pyfunction]
#[pyo3(signature = (moving, fixed, out_prefix, *, moving_t2 = None, fixed_t2 = None, extra = Vec::new(), binary = "antsRegistration"))]
pub fn ants_syn_argv(
    moving: &str,
    fixed: &str,
    out_prefix: &str,
    moving_t2: Option<&str>,
    fixed_t2: Option<&str>,
    extra: Vec<String>,
    binary: &str,
) -> PyResult<Vec<String>> {
    match (moving_t2, fixed_t2) {
        (None, None) => Ok(ants_argv::build_ants_syn_argv(
            path(binary),
            path(moving),
            path(fixed),
            path(out_prefix),
            &extra,
        )),
        (Some(m2), Some(f2)) => Ok(ants_argv::build_ants_syn_multispectral_argv(
            path(binary),
            path(moving),
            path(fixed),
            path(m2),
            path(f2),
            path(out_prefix),
            &extra,
        )),
        _ => Err(PyValueError::new_err(
            "moving_t2 and fixed_t2 must be given together",
        )),
    }
}

pub fn init_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("BidsError", m.py().get_type::<PyBidsError>())?;
    m.add_function(wrap_pyfunction!(reconstruct_ct_py, m)?)?;
    m.add_function(wrap_pyfunction!(gpu_probe, m)?)?;
    m.add_function(wrap_pyfunction!(plan_run, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_subject, m)?)?;
    m.add_function(wrap_pyfunction!(ants_coreg_argv, m)?)?;
    m.add_function(wrap_pyfunction!(ants_syn_argv, m)?)?;
    Ok(())
}

#[pymodule]
#[pyo3(name = "dbsloc_reconstruct")]
pub fn dbsloc_reconstruct_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    init_module(m)
}
