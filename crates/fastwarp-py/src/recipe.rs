// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The registration recipe as a Python dict. The dict is converted to JSON and read by the
//! command line's recipe parser (`fastwarp_cli::recipe`), so both front ends accept the same two
//! forms (`{"rigid": ..., "affine": ..., "syn": ...}` and `{"initial": ..., "stages": [...]}`,
//! both with `"winsorize"`) with the same keys, defaults, checks and messages. The single linear
//! stage of `LinearSession.register` keeps its own dict reader below.

use fastwarp_cli::recipe::{
    check_linear, cohort_linear, parse_units, units_name, Recipe, LINEAR_KEYS,
};
use fastwarp_reg::linear::LinearRecipe;
use fastwarp_reg::pyramid::SigmaUnits;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};

fn value_err(msg: String) -> PyErr {
    PyValueError::new_err(msg)
}

/// A Python value as JSON: dicts with string keys, lists and tuples, strings, bools, integers,
/// finite floats, `None`, and an `os.PathLike` as its string. `what` names the value in errors.
fn to_json(v: &Bound<'_, PyAny>, what: &str) -> PyResult<Value> {
    if v.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(b) = v.cast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if v.is_instance_of::<PyInt>() {
        return int_json(v, what);
    }
    if v.is_instance_of::<PyFloat>() {
        return float_json(v, what);
    }
    if let Ok(s) = v.cast::<PyString>() {
        return Ok(Value::String(s.to_str()?.to_string()));
    }
    if let Ok(d) = v.cast::<PyDict>() {
        let mut m = Map::new();
        for (k, item) in d.iter() {
            let key: String = k
                .extract()
                .map_err(|_| value_err(format!("{what}: keys must be strings")))?;
            let inner = format!("{what}['{key}']");
            m.insert(key, to_json(&item, &inner)?);
        }
        return Ok(Value::Object(m));
    }
    if v.is_instance_of::<PyList>() || v.is_instance_of::<PyTuple>() {
        let mut out = Vec::new();
        for (i, item) in v.try_iter()?.enumerate() {
            out.push(to_json(&item?, &format!("{what}[{i}]"))?);
        }
        return Ok(Value::Array(out));
    }
    if v.hasattr("__fspath__")? {
        let p = v.call_method0("__fspath__")?;
        if let Ok(s) = p.cast::<PyString>() {
            return Ok(Value::String(s.to_str()?.to_string()));
        }
    }
    if v.hasattr("tolist")? {
        return to_json(&v.call_method0("tolist")?, what);
    }
    if v.hasattr("__index__")? {
        return int_json(v, what);
    }
    if v.hasattr("__float__")? {
        return float_json(v, what);
    }
    let name = v
        .get_type()
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "value".to_string());
    Err(value_err(format!("{what}: a {name} is not a recipe value")))
}

fn int_json(v: &Bound<'_, PyAny>, what: &str) -> PyResult<Value> {
    if let Ok(n) = v.extract::<u64>() {
        return Ok(Value::Number(n.into()));
    }
    if let Ok(n) = v.extract::<i64>() {
        return Ok(Value::Number(n.into()));
    }
    Err(value_err(format!("{what}: integer {v} is out of range")))
}

fn float_json(v: &Bound<'_, PyAny>, what: &str) -> PyResult<Value> {
    let f: f64 = v
        .extract()
        .map_err(|_| value_err(format!("{what}: {v} is not a number")))?;
    Number::from_f64(f).map(Value::Number).ok_or_else(|| {
        value_err(format!(
            "{what}: got {f}, but values must be finite numbers"
        ))
    })
}

/// JSON as the Python value `json.loads` would give.
pub fn from_json<'py>(py: Python<'py>, v: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else {
                n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any()
            }
        }
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(a) => {
            let l = PyList::empty(py);
            for item in a {
                l.append(from_json(py, item)?)?;
            }
            l.into_any()
        }
        Value::Object(m) => {
            let d = PyDict::new(py);
            for (k, item) in m {
                d.set_item(k, from_json(py, item)?)?;
            }
            d.into_any()
        }
    })
}

/// The recipe `d` describes (the cohort recipe with the stages and fields it names replaced),
/// checked for `channels` channels and for what the GPU kernels support.
pub fn from_dict(d: Option<&Bound<'_, PyDict>>, channels: usize) -> PyResult<Recipe> {
    match d {
        None => {
            let r = Recipe::cohort(channels);
            r.check(channels).map_err(value_err)?;
            Ok(r)
        }
        Some(d) => Recipe::from_json(&to_json(d.as_any(), "recipe")?, channels).map_err(value_err),
    }
}

/// The recipe as the dict [`from_dict`] reads (every field present, in the form it was given).
pub fn to_dict<'py>(py: Python<'py>, r: &Recipe) -> PyResult<Bound<'py, PyDict>> {
    Ok(from_json(py, &r.to_json())?.cast_into::<PyDict>()?)
}

/// One `Rigid` (`transform = "rigid"`) or `Affine` (`"affine"`) stage: the cohort's, with the
/// fields `d` names replaced, checked like a recipe.
pub fn linear_stage(d: Option<&Bound<'_, PyDict>>, transform: &str) -> PyResult<LinearRecipe> {
    let mut stage = cohort_linear(transform).map_err(value_err)?;
    if let Some(d) = d {
        linear_from(d, &mut stage, "stage")?;
    }
    check_linear("stage", &stage).map_err(value_err)?;
    Ok(stage)
}

/// The stage as the dict [`linear_stage`] reads.
pub fn linear_stage_dict<'py>(
    py: Python<'py>,
    stage: &LinearRecipe,
) -> PyResult<Bound<'py, PyDict>> {
    linear_dict(py, stage)
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
    *slot = parse_units(&name, what).map_err(value_err)?;
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
