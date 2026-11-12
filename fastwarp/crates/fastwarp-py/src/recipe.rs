// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The registration recipe as a Python dict: `{"rigid": {...}, "affine": {...}, "syn": {...}}`,
//! each field of [`LinearRecipe`] / [`SynRecipe`] under its own name. A missing stage or field
//! keeps the dbs-localizer cohort value, an unknown one is an error, so a caller (leaddbs-py) can
//! state the whole schedule and a typo never passes silently.

use fastwarp_reg::linear::LinearRecipe;
use fastwarp_reg::pyramid::SigmaUnits;
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::LinearPlan;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Histogram bins the GPU Mattes MI kernels are built for.
const GPU_MI_BINS: usize = 32;
/// Largest CC radius of the GPU box-sum kernels.
const GPU_CC_RADIUS_MAX: usize = 8;

const LINEAR_KEYS: &[&str] = &[
    "gradient_step",
    "iterations",
    "convergence_threshold",
    "convergence_window",
    "shrink_factors",
    "smoothing_sigmas",
    "sigma_units",
    "bins",
    "sampling_percentage",
    "seed",
];
const SYN_KEYS: &[&str] = &[
    "gradient_step",
    "update_field_variance",
    "total_field_variance",
    "cc_radius",
    "channel_weights",
    "iterations",
    "convergence_threshold",
    "convergence_window",
    "shrink_factors",
    "smoothing_sigmas",
    "sigma_units",
];

/// The three stages that run: Rigid, Affine (Mattes MI, first channel) and SyN (CC, all channels).
pub struct Recipe {
    pub linear: LinearPlan,
    pub syn: SynRecipe,
}

impl Recipe {
    /// The dbs-localizer cohort recipe for `channels` channels (weights 1, or 0.5 each for two).
    pub fn cohort(channels: usize) -> Self {
        Self {
            linear: LinearPlan::cohort(),
            syn: if channels == 2 {
                SynRecipe::cohort_multispectral()
            } else {
                SynRecipe::cohort()
            },
        }
    }

    /// The cohort recipe with the stages and fields `d` names replaced, checked for `channels`
    /// channels and for what the GPU kernels support.
    pub fn from_dict(d: Option<&Bound<'_, PyDict>>, channels: usize) -> PyResult<Self> {
        let mut r = Self::cohort(channels);
        if let Some(d) = d {
            check_keys(d, &["rigid", "affine", "syn"], "recipe")?;
            if let Some(s) = stage(d, "rigid")? {
                linear_from(&s, &mut r.linear.rigid, "recipe['rigid']")?;
            }
            if let Some(s) = stage(d, "affine")? {
                linear_from(&s, &mut r.linear.affine, "recipe['affine']")?;
            }
            if let Some(s) = stage(d, "syn")? {
                syn_from(&s, &mut r.syn)?;
            }
        }
        r.check(channels)?;
        Ok(r)
    }

    fn check(&self, channels: usize) -> PyResult<()> {
        check_linear("recipe['rigid']", &self.linear.rigid)?;
        check_linear("recipe['affine']", &self.linear.affine)?;
        self.syn
            .validate()
            .map_err(|e| PyValueError::new_err(format!("recipe['syn']: {e}")))?;
        if self.syn.cc_radius > GPU_CC_RADIUS_MAX {
            return Err(PyValueError::new_err(format!(
                "recipe['syn']['cc_radius']: the GPU kernels support 1 to {GPU_CC_RADIUS_MAX}, \
                 not {}",
                self.syn.cc_radius
            )));
        }
        if self.syn.channel_weights.len() != channels {
            return Err(PyValueError::new_err(format!(
                "recipe['syn']['channel_weights'] has {} weights for {channels} channel(s)",
                self.syn.channel_weights.len()
            )));
        }
        Ok(())
    }

    /// The recipe as the dict [`Recipe::from_dict`] reads (every field present).
    pub fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        d.set_item("rigid", linear_dict(py, &self.linear.rigid)?)?;
        d.set_item("affine", linear_dict(py, &self.linear.affine)?)?;
        let s = PyDict::new(py);
        let r = &self.syn;
        s.set_item("gradient_step", r.gradient_step)?;
        s.set_item("update_field_variance", r.update_field_variance)?;
        s.set_item("total_field_variance", r.total_field_variance)?;
        s.set_item("cc_radius", r.cc_radius)?;
        s.set_item("channel_weights", r.channel_weights.clone())?;
        s.set_item("iterations", r.iterations.clone())?;
        s.set_item("convergence_threshold", r.convergence_threshold)?;
        s.set_item("convergence_window", r.convergence_window)?;
        s.set_item("shrink_factors", r.shrink_factors.clone())?;
        s.set_item("smoothing_sigmas", r.smoothing_sigmas.clone())?;
        s.set_item("sigma_units", units_name(r.sigma_units))?;
        d.set_item("syn", s)?;
        Ok(d)
    }
}

/// One `Rigid` (`transform = "rigid"`) or `Affine` (`"affine"`) stage: the cohort's, with the
/// fields `d` names replaced, checked like a recipe.
pub fn linear_stage(d: Option<&Bound<'_, PyDict>>, transform: &str) -> PyResult<LinearRecipe> {
    let mut stage = match transform {
        "rigid" => LinearRecipe::cohort_rigid(),
        "affine" => LinearRecipe::cohort_affine(),
        other => {
            return Err(PyValueError::new_err(format!(
                "transform must be 'rigid' or 'affine', not {other:?}"
            )))
        }
    };
    if let Some(d) = d {
        linear_from(d, &mut stage, "stage")?;
    }
    check_linear("stage", &stage)?;
    Ok(stage)
}

/// The stage as the dict [`linear_stage`] reads.
pub fn linear_stage_dict<'py>(
    py: Python<'py>,
    stage: &LinearRecipe,
) -> PyResult<Bound<'py, PyDict>> {
    linear_dict(py, stage)
}

fn check_linear(what: &str, l: &LinearRecipe) -> PyResult<()> {
    l.validate()
        .map_err(|e| PyValueError::new_err(format!("{what}: {e}")))?;
    if l.bins != GPU_MI_BINS {
        return Err(PyValueError::new_err(format!(
            "{what}['bins']: the GPU MI kernels support {GPU_MI_BINS} bins, not {}",
            l.bins
        )));
    }
    Ok(())
}

fn linear_dict<'py>(py: Python<'py>, l: &LinearRecipe) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("gradient_step", l.gradient_step)?;
    d.set_item("iterations", l.iterations.clone())?;
    d.set_item("convergence_threshold", l.convergence_threshold)?;
    d.set_item("convergence_window", l.convergence_window)?;
    d.set_item("shrink_factors", l.shrink_factors.clone())?;
    d.set_item("smoothing_sigmas", l.smoothing_sigmas.clone())?;
    d.set_item("sigma_units", units_name(l.sigma_units))?;
    d.set_item("bins", l.bins)?;
    d.set_item("sampling_percentage", f64::from(l.sampling_percentage))?;
    d.set_item("seed", l.seed)?;
    Ok(d)
}

fn units_name(u: SigmaUnits) -> &'static str {
    match u {
        SigmaUnits::Voxels => "vox",
        SigmaUnits::Millimetres => "mm",
    }
}

fn check_keys(d: &Bound<'_, PyDict>, allowed: &[&str], what: &str) -> PyResult<()> {
    for key in d.keys() {
        let k: String = key
            .extract()
            .map_err(|_| PyValueError::new_err(format!("{what}: keys must be strings")))?;
        if !allowed.contains(&k.as_str()) {
            return Err(PyValueError::new_err(format!(
                "{what}: unknown key '{k}' (known: {})",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn stage<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
    match d.get_item(key)? {
        None => Ok(None),
        Some(v) if v.is_none() => Ok(None),
        Some(v) => v
            .cast_into::<PyDict>()
            .map(Some)
            .map_err(|_| PyValueError::new_err(format!("recipe['{key}'] must be a dict"))),
    }
}

/// Replaces `slot` with `d[key]` when present; a value of the wrong type is a `ValueError`
/// naming the field.
fn set<'py, T: FromPyObjectOwned<'py>>(
    d: &Bound<'py, PyDict>,
    key: &str,
    what: &str,
    slot: &mut T,
) -> PyResult<()> {
    if let Some(v) = d.get_item(key)? {
        // A string is a sequence too: refuse it where a list is meant.
        if v.is_instance_of::<pyo3::types::PyString>() && key != "sigma_units" {
            return Err(PyValueError::new_err(format!(
                "{what}['{key}']: got a string"
            )));
        }
        *slot = v.extract::<T>().map_err(|e| {
            let e: PyErr = e.into();
            PyValueError::new_err(format!("{what}['{key}']: {}", e.value(d.py())))
        })?;
    }
    Ok(())
}

fn units(d: &Bound<'_, PyDict>, what: &str, slot: &mut SigmaUnits) -> PyResult<()> {
    let mut name = units_name(*slot).to_string();
    set(d, "sigma_units", what, &mut name)?;
    *slot = match name.as_str() {
        "vox" => SigmaUnits::Voxels,
        "mm" => SigmaUnits::Millimetres,
        other => {
            return Err(PyValueError::new_err(format!(
                "{what}['sigma_units']: 'vox' or 'mm', not '{other}'"
            )))
        }
    };
    Ok(())
}

fn linear_from(d: &Bound<'_, PyDict>, l: &mut LinearRecipe, what: &str) -> PyResult<()> {
    check_keys(d, LINEAR_KEYS, what)?;
    set(d, "gradient_step", what, &mut l.gradient_step)?;
    set(d, "iterations", what, &mut l.iterations)?;
    set(
        d,
        "convergence_threshold",
        what,
        &mut l.convergence_threshold,
    )?;
    set(d, "convergence_window", what, &mut l.convergence_window)?;
    set(d, "shrink_factors", what, &mut l.shrink_factors)?;
    set(d, "smoothing_sigmas", what, &mut l.smoothing_sigmas)?;
    units(d, what, &mut l.sigma_units)?;
    set(d, "bins", what, &mut l.bins)?;
    let mut sampling = f64::from(l.sampling_percentage);
    set(d, "sampling_percentage", what, &mut sampling)?;
    l.sampling_percentage = sampling as f32;
    set(d, "seed", what, &mut l.seed)?;
    Ok(())
}

fn syn_from(d: &Bound<'_, PyDict>, r: &mut SynRecipe) -> PyResult<()> {
    let what = "recipe['syn']";
    check_keys(d, SYN_KEYS, what)?;
    set(d, "gradient_step", what, &mut r.gradient_step)?;
    set(
        d,
        "update_field_variance",
        what,
        &mut r.update_field_variance,
    )?;
    set(d, "total_field_variance", what, &mut r.total_field_variance)?;
    set(d, "cc_radius", what, &mut r.cc_radius)?;
    set(d, "channel_weights", what, &mut r.channel_weights)?;
    set(d, "iterations", what, &mut r.iterations)?;
    set(
        d,
        "convergence_threshold",
        what,
        &mut r.convergence_threshold,
    )?;
    set(d, "convergence_window", what, &mut r.convergence_window)?;
    set(d, "shrink_factors", what, &mut r.shrink_factors)?;
    set(d, "smoothing_sigmas", what, &mut r.smoothing_sigmas)?;
    units(d, what, &mut r.sigma_units)?;
    Ok(())
}
