// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

//! The registration recipe, in two forms. The cohort form `{"rigid": {...}, "affine": {...},
//! "syn": {...}}` is the dbs-localizer chain (centre of mass, Rigid, Affine, SyN), each field of
//! [`LinearRecipe`] / [`SynRecipe`] under its own name. The stage-list form `{"initial": ...,
//! "stages": [...]}` names the start and any number of `rigid` / `affine` stages followed by at
//! most one `syn` stage. Both take `"winsorize"`. A missing stage or field keeps the cohort value,
//! an unknown one is an error, so a caller can state the whole schedule and a typo never passes
//! silently. The checks are shared by the Python dict (converted to JSON) and the JSON file of
//! the command line.

use std::path::PathBuf;

use fastwarp_reg::linear::{LinearKind, LinearRecipe};
use fastwarp_reg::pyramid::SigmaUnits;
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::{LinearPlan, WINSORIZE_LOWER, WINSORIZE_UPPER};
use fastwarp_reg::stages::{InitialTransform, StagePlan};
use fastwarp_reg::transform::{read_transform_file, AffineTransform, TransformElem};
use serde_json::{json, Map, Value};

/// Histogram bins the GPU Mattes MI kernels are built for.
pub const GPU_MI_BINS: usize = 32;
/// Largest CC radius of the GPU box-sum kernels.
pub const GPU_CC_RADIUS_MAX: usize = 8;

/// The stages of a cohort-form recipe.
pub const STAGE_KEYS: &[&str] = &["rigid", "affine", "syn"];

/// The keys of the stage-list form.
pub const STAGE_LIST_KEYS: &[&str] = &["initial", "stages"];

/// Every top-level key of a recipe.
pub const RECIPE_KEYS: &[&str] = &["rigid", "affine", "syn", "initial", "stages", "winsorize"];

/// The default winsorizing quantiles as a recipe reports them.
pub const DEFAULT_WINSORIZE: (f64, f64) = (0.005, 0.995);

/// The fields of a `rigid` or `affine` stage.
pub const LINEAR_KEYS: &[&str] = &[
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

/// The fields of the `syn` stage.
pub const SYN_KEYS: &[&str] = &[
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

/// How the start of a stage-list recipe was given, to report it back.
#[derive(Clone, Debug, PartialEq)]
pub enum InitialSpec {
    /// `"com"`.
    CentreOfMass,
    /// `"identity"`.
    Identity,
    /// `{"path": ..., "invert": ...}`.
    File { path: PathBuf, invert: bool },
    /// `{"parameters": ..., "center": ...}`.
    Parameters,
}

/// The stages that run, with the form the recipe was given in.
#[derive(Clone, Debug)]
pub struct Recipe {
    /// What runs.
    pub plan: StagePlan,
    /// Whether the recipe used the stage-list form (reported back in that form).
    pub stage_list: bool,
    /// How the start was given.
    pub initial: InitialSpec,
    /// The winsorizing quantiles as given (`None`: ANTs' plain rescale to [0, 1]).
    pub winsorize: Option<(f64, f64)>,
}

impl Recipe {
    /// The dbs-localizer cohort recipe for `channels` channels (weights 1, or 0.5 each for two).
    pub fn cohort(channels: usize) -> Self {
        Self {
            plan: StagePlan::cohort(&LinearPlan::cohort(), &cohort_syn(channels)),
            stage_list: false,
            initial: InitialSpec::CentreOfMass,
            winsorize: Some(DEFAULT_WINSORIZE),
        }
    }

    fn linear_name(&self, index: usize) -> String {
        if self.stage_list {
            format!("recipe['stages'][{index}]")
        } else if index == 0 {
            "recipe['rigid']".to_string()
        } else {
            "recipe['affine']".to_string()
        }
    }

    fn syn_name(&self) -> String {
        if self.stage_list {
            format!("recipe['stages'][{}]", self.plan.linear.len())
        } else {
            "recipe['syn']".to_string()
        }
    }

    /// Checks the recipe for `channels` channels and for what the GPU kernels support.
    pub fn check(&self, channels: usize) -> Result<(), String> {
        if self.plan.linear.is_empty() && self.plan.syn.is_none() {
            return Err("recipe['stages'] is empty: give at least one stage".to_string());
        }
        if self.plan.syn.is_none() && channels > 1 {
            return Err(format!(
                "recipe['stages'] has no SyN stage, which the second channel is for: the linear \
                 stages register the first channel only; give one fixed and one moving image, \
                 not {channels}"
            ));
        }
        for (i, l) in self.plan.linear.iter().enumerate() {
            check_linear(&self.linear_name(i), l)?;
        }
        if let Some(syn) = &self.plan.syn {
            let what = self.syn_name();
            syn.validate().map_err(|e| format!("{what}: {e}"))?;
            if syn.cc_radius > GPU_CC_RADIUS_MAX {
                return Err(format!(
                    "{what}['cc_radius']: the GPU kernels support 1 to {GPU_CC_RADIUS_MAX}, \
                     not {}",
                    syn.cc_radius
                ));
            }
            if syn.channel_weights.len() != channels {
                return Err(format!(
                    "{what}['channel_weights'] has {} weights for {channels} channel(s)",
                    syn.channel_weights.len()
                ));
            }
        }
        if let InitialTransform::Affine(a) = &self.plan.initial {
            let finite = a
                .parameters()
                .iter()
                .chain(&a.center)
                .all(|v| v.is_finite());
            if !finite {
                return Err(
                    "the initial transform in recipe['initial'] has non-finite values".to_string(),
                );
            }
            if a.inverse().is_err() {
                return Err("the initial transform in recipe['initial'] is singular".to_string());
            }
        }
        if fastwarp_reg::register::check_winsorize(self.plan.winsorize).is_err() {
            return Err(winsorize_message());
        }
        self.plan.validate().map_err(|e| format!("recipe: {e}"))
    }

    /// The cohort recipe with the stages and fields of the JSON object `v` replaced, checked
    /// like [`Recipe::check`]. A start given as a file is read here (a relative path is taken
    /// from the current folder; [`resolve_initial_path`] anchors it elsewhere first).
    pub fn from_json(v: &Value, channels: usize) -> Result<Self, String> {
        let mut r = Self::cohort(channels);
        let obj = v
            .as_object()
            .ok_or_else(|| "recipe: must be a dict (a JSON object)".to_string())?;
        check_json_keys(obj, RECIPE_KEYS, "recipe")?;
        let given = |k: &str| obj.get(k).is_some_and(|v| !v.is_null());
        let first = |keys: &[&'static str]| keys.iter().find(|k| given(k)).copied();
        if let (Some(old), Some(new)) = (first(STAGE_KEYS), first(STAGE_LIST_KEYS)) {
            return Err(format!(
                "recipe: '{old}' cannot be combined with '{new}'; with 'initial' / 'stages' \
                 give every stage in recipe['stages']"
            ));
        }
        if let Some(w) = obj.get("winsorize") {
            r.winsorize = winsorize_from_json(w)?;
            r.plan.winsorize = r.winsorize.map(|(lo, hi)| (lo as f32, hi as f32));
        }
        if first(STAGE_LIST_KEYS).is_some() {
            r.stage_list = true;
            if let Some(v) = obj.get("initial").filter(|v| !v.is_null()) {
                let (initial, spec) = initial_from_json(v)?;
                r.plan.initial = initial;
                r.initial = spec;
            }
            if let Some(v) = obj.get("stages").filter(|v| !v.is_null()) {
                let (linear, syn) = stages_from_json(v, channels)?;
                r.plan.linear = linear;
                r.plan.syn = syn;
            }
        } else {
            let mut linear = LinearPlan::cohort();
            let mut syn = cohort_syn(channels);
            for key in STAGE_KEYS {
                let stage = match obj.get(*key) {
                    None | Some(Value::Null) => continue,
                    Some(Value::Object(m)) => m,
                    Some(_) => {
                        return Err(format!("recipe['{key}'] must be a dict (a JSON object)"))
                    }
                };
                let what = format!("recipe['{key}']");
                match *key {
                    "rigid" => linear_from_json(stage, &mut linear.rigid, &what)?,
                    "affine" => linear_from_json(stage, &mut linear.affine, &what)?,
                    _ => syn_from_json(stage, &mut syn, &what)?,
                }
            }
            r.plan.linear = vec![linear.rigid, linear.affine];
            r.plan.syn = Some(syn);
        }
        r.check(channels)?;
        Ok(r)
    }

    /// The recipe as the JSON object [`Recipe::from_json`] reads, in the form it was given, every
    /// field present. The cohort form reports `winsorize` only when it is not the default, so
    /// [`Recipe::cohort`] gives the dict `fastwarp.cohort_recipe()` returns.
    pub fn to_json(&self) -> Value {
        let winsorize = match self.winsorize {
            Some((lo, hi)) => json!([lo, hi]),
            None => Value::Null,
        };
        if self.stage_list {
            let mut stages: Vec<Value> = self
                .plan
                .linear
                .iter()
                .map(|l| {
                    let mut s = linear_to_json(l);
                    s["transform"] = json!(kind_name(l.kind));
                    s
                })
                .collect();
            if let Some(syn) = &self.plan.syn {
                let mut s = syn_to_json(syn);
                s["transform"] = json!("syn");
                stages.push(s);
            }
            return json!({
                "initial": self.initial_to_json(),
                "stages": stages,
                "winsorize": winsorize,
            });
        }
        let mut v = json!({
            "rigid": linear_to_json(&self.plan.linear[0]),
            "affine": linear_to_json(&self.plan.linear[1]),
        });
        if let Some(syn) = &self.plan.syn {
            v["syn"] = syn_to_json(syn);
        }
        if self.plan.winsorize != Some((WINSORIZE_LOWER, WINSORIZE_UPPER)) {
            v["winsorize"] = winsorize;
        }
        v
    }

    fn initial_to_json(&self) -> Value {
        match &self.initial {
            InitialSpec::CentreOfMass => json!("com"),
            InitialSpec::Identity => json!("identity"),
            InitialSpec::File { path, invert } => {
                json!({"path": path.to_string_lossy(), "invert": invert})
            }
            InitialSpec::Parameters => match &self.plan.initial {
                InitialTransform::Affine(a) => {
                    json!({"parameters": a.parameters(), "center": a.center})
                }
                _ => Value::Null,
            },
        }
    }
}

/// `v` with a relative `initial.path` joined to `base` (the folder of a recipe file), so the file
/// is found wherever the program runs; other values are returned unchanged.
pub fn resolve_initial_path(v: &Value, base: &std::path::Path) -> Value {
    let mut out = v.clone();
    if let Some(Value::String(p)) = out.pointer_mut("/initial/path") {
        let path = PathBuf::from(p.as_str());
        if path.is_relative() {
            *p = base.join(path).to_string_lossy().into_owned();
        }
    }
    out
}

fn cohort_syn(channels: usize) -> SynRecipe {
    if channels == 2 {
        SynRecipe::cohort_multispectral()
    } else {
        SynRecipe::cohort()
    }
}

/// `"rigid"` or `"affine"`.
pub fn kind_name(kind: LinearKind) -> &'static str {
    match kind {
        LinearKind::Rigid => "rigid",
        LinearKind::Affine => "affine",
    }
}

fn winsorize_message() -> String {
    "recipe['winsorize']: None (null) or [lower, upper] quantiles with 0 <= lower < upper <= 1"
        .to_string()
}

fn winsorize_from_json(v: &Value) -> Result<Option<(f64, f64)>, String> {
    if v.is_null() {
        return Ok(None);
    }
    match v
        .as_array()
        .map(|a| a.iter().map(Value::as_f64).collect::<Vec<_>>())
    {
        Some(q) => match q.as_slice() {
            [Some(lo), Some(hi)] if 0.0 <= *lo && lo < hi && *hi <= 1.0 => Ok(Some((*lo, *hi))),
            _ => Err(winsorize_message()),
        },
        None => Err(winsorize_message()),
    }
}

fn initial_error(got: &Value) -> String {
    format!(
        "recipe['initial']: expected 'com', 'identity', {{'path': FILE, 'invert': BOOL}} or \
         {{'parameters': [12 numbers], 'center': [3 numbers]}}, got {got}"
    )
}

fn initial_from_json(v: &Value) -> Result<(InitialTransform, InitialSpec), String> {
    match v {
        Value::String(s) if s == "com" => {
            Ok((InitialTransform::CentreOfMass, InitialSpec::CentreOfMass))
        }
        Value::String(s) if s == "identity" => {
            Ok((InitialTransform::Identity, InitialSpec::Identity))
        }
        Value::Object(m) if m.contains_key("path") => {
            check_json_keys(m, &["path", "invert"], "recipe['initial']")?;
            let path = m
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| initial_error(v))?;
            let invert = match m.get("invert") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(other) => {
                    return Err(format!(
                        "recipe['initial']['invert']: expected true or false, got {other}"
                    ))
                }
            };
            initial_file(PathBuf::from(path), invert)
        }
        Value::Object(m) => {
            check_json_keys(m, &["parameters", "center"], "recipe['initial']")?;
            let numbers = |k: &str, n: usize| -> Result<Vec<f64>, String> {
                let list = m
                    .get(k)
                    .and_then(Value::as_array)
                    .ok_or_else(|| initial_error(v))?;
                let out: Vec<f64> = list.iter().filter_map(Value::as_f64).collect();
                if out.len() != n || list.len() != n {
                    return Err(format!("recipe['initial']['{k}']: expected {n} numbers"));
                }
                Ok(out)
            };
            let a = AffineTransform::from_parameters(
                &numbers("parameters", 12)?,
                &numbers("center", 3)?,
            )
            .map_err(|e| format!("recipe['initial']: {e}"))?;
            Ok((InitialTransform::Affine(a), InitialSpec::Parameters))
        }
        other => Err(initial_error(other)),
    }
}

/// The one affine transform of `path` (inverted with `invert`), the start of a chain.
pub fn initial_file(
    path: PathBuf,
    invert: bool,
) -> Result<(InitialTransform, InitialSpec), String> {
    let what = format!("recipe['initial']: {}", path.display());
    let mut chain = read_transform_file(&path).map_err(|e| format!("{what}: {e}"))?;
    if invert {
        chain = chain
            .inverse()
            .map_err(|e| format!("{what}: cannot invert it ({e})"))?;
    }
    match chain.queue() {
        [TransformElem::Affine(a)] => Ok((
            InitialTransform::Affine(*a),
            InitialSpec::File { path, invert },
        )),
        [TransformElem::DisplacementField(_)] => Err(format!(
            "{what} holds a displacement field: FastWarp starts from one affine (.mat, .txt)"
        )),
        q => Err(format!(
            "{what} holds {} transforms: FastWarp starts from one affine (.mat, .txt)",
            q.len()
        )),
    }
}

fn stages_from_json(
    v: &Value,
    channels: usize,
) -> Result<(Vec<LinearRecipe>, Option<SynRecipe>), String> {
    let list = v
        .as_array()
        .ok_or_else(|| format!("recipe['stages'] must be a list of stage dicts, got {v}"))?;
    let mut linear = Vec::new();
    let mut syn: Option<SynRecipe> = None;
    for (i, item) in list.iter().enumerate() {
        let what = format!("recipe['stages'][{i}]");
        let m = item
            .as_object()
            .ok_or_else(|| format!("{what} must be a dict (a JSON object)"))?;
        if syn.is_some() {
            return Err(format!(
                "{what}: a stage after the SyN stage is not supported: FastWarp runs the linear \
                 stages first and at most one SyN stage, last"
            ));
        }
        let kind = m.get("transform").and_then(Value::as_str);
        let mut fields = m.clone();
        fields.remove("transform");
        match kind {
            Some("rigid") => {
                let mut l = LinearRecipe::cohort_rigid();
                linear_from_json(&fields, &mut l, &what)?;
                linear.push(l);
            }
            Some("affine") => {
                let mut l = LinearRecipe::cohort_affine();
                linear_from_json(&fields, &mut l, &what)?;
                linear.push(l);
            }
            Some("syn") => {
                let mut r = cohort_syn(channels);
                syn_from_json(&fields, &mut r, &what)?;
                syn = Some(r);
            }
            _ => {
                return Err(format!(
                    "{what}['transform'] must be 'rigid', 'affine' or 'syn', got {}",
                    m.get("transform").cloned().unwrap_or(Value::Null)
                ))
            }
        }
    }
    Ok((linear, syn))
}

/// The cohort `Rigid` (`transform = "rigid"`) or `Affine` (`"affine"`) stage.
pub fn cohort_linear(transform: &str) -> Result<LinearRecipe, String> {
    match transform {
        "rigid" => Ok(LinearRecipe::cohort_rigid()),
        "affine" => Ok(LinearRecipe::cohort_affine()),
        other => Err(format!(
            "transform must be 'rigid' or 'affine', not {other:?}"
        )),
    }
}

/// Checks one linear stage, `what` naming it in the message.
pub fn check_linear(what: &str, l: &LinearRecipe) -> Result<(), String> {
    l.validate().map_err(|e| format!("{what}: {e}"))?;
    if l.bins != GPU_MI_BINS {
        return Err(format!(
            "{what}['bins']: the GPU MI kernels support {GPU_MI_BINS} bins, not {}",
            l.bins
        ));
    }
    Ok(())
}

/// One linear stage: the cohort's for `transform`, with the fields of the JSON object `v`
/// replaced, checked like a recipe.
pub fn linear_stage_from_json(v: Option<&Value>, transform: &str) -> Result<LinearRecipe, String> {
    let mut stage = cohort_linear(transform)?;
    match v {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => linear_from_json(m, &mut stage, "stage")?,
        Some(_) => return Err("stage: must be a dict (a JSON object)".to_string()),
    }
    check_linear("stage", &stage)?;
    Ok(stage)
}

/// The name of a sigma unit in a recipe (`vox` or `mm`).
pub fn units_name(u: SigmaUnits) -> &'static str {
    match u {
        SigmaUnits::Voxels => "vox",
        SigmaUnits::Millimetres => "mm",
    }
}

/// The sigma unit of a recipe name, `what` naming the field in the message.
pub fn parse_units(name: &str, what: &str) -> Result<SigmaUnits, String> {
    match name {
        "vox" => Ok(SigmaUnits::Voxels),
        "mm" => Ok(SigmaUnits::Millimetres),
        other => Err(format!(
            "{what}['sigma_units']: 'vox' or 'mm', not '{other}'"
        )),
    }
}

/// A linear stage as JSON, the fields [`linear_stage_from_json`] reads.
pub fn linear_to_json(l: &LinearRecipe) -> Value {
    json!({
        "gradient_step": l.gradient_step,
        "iterations": l.iterations,
        "convergence_threshold": l.convergence_threshold,
        "convergence_window": l.convergence_window,
        "shrink_factors": l.shrink_factors,
        "smoothing_sigmas": l.smoothing_sigmas,
        "sigma_units": units_name(l.sigma_units),
        "bins": l.bins,
        "sampling_percentage": f64::from(l.sampling_percentage),
        "seed": l.seed,
    })
}

/// The SyN stage as JSON, the fields of a recipe's `syn` entry.
pub fn syn_to_json(r: &SynRecipe) -> Value {
    json!({
        "gradient_step": r.gradient_step,
        "update_field_variance": r.update_field_variance,
        "total_field_variance": r.total_field_variance,
        "cc_radius": r.cc_radius,
        "channel_weights": r.channel_weights,
        "iterations": r.iterations,
        "convergence_threshold": r.convergence_threshold,
        "convergence_window": r.convergence_window,
        "shrink_factors": r.shrink_factors,
        "smoothing_sigmas": r.smoothing_sigmas,
        "sigma_units": units_name(r.sigma_units),
    })
}

fn check_json_keys(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    for k in m.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!(
                "{what}: unknown key '{k}' (known: {})",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn field_err(what: &str, key: &str, want: &str, v: &Value) -> String {
    format!("{what}['{key}']: expected {want}, got {v}")
}

fn f64_of(what: &str, key: &str, v: &Value) -> Result<f64, String> {
    v.as_f64()
        .ok_or_else(|| field_err(what, key, "a number", v))
}

fn uint_of(what: &str, key: &str, v: &Value, max: u64) -> Result<u64, String> {
    v.as_u64()
        .filter(|n| *n <= max)
        .ok_or_else(|| field_err(what, key, "a non-negative integer", v))
}

fn list_of<'a>(what: &str, key: &str, v: &'a Value) -> Result<&'a Vec<Value>, String> {
    v.as_array().ok_or_else(|| match v {
        Value::String(_) => format!("{what}['{key}']: expected a list, got a string {v}"),
        _ => field_err(what, key, "a list", v),
    })
}

fn set_f64(m: &Map<String, Value>, what: &str, key: &str, slot: &mut f64) -> Result<(), String> {
    if let Some(v) = m.get(key) {
        *slot = f64_of(what, key, v)?;
    }
    Ok(())
}

fn set_usize(
    m: &Map<String, Value>,
    what: &str,
    key: &str,
    slot: &mut usize,
) -> Result<(), String> {
    if let Some(v) = m.get(key) {
        *slot = usize::try_from(uint_of(what, key, v, u64::MAX)?)
            .map_err(|_| field_err(what, key, "a smaller integer", v))?;
    }
    Ok(())
}

fn set_u32(m: &Map<String, Value>, what: &str, key: &str, slot: &mut u32) -> Result<(), String> {
    if let Some(v) = m.get(key) {
        *slot = uint_of(what, key, v, u64::from(u32::MAX))? as u32;
    }
    Ok(())
}

fn set_u32s(
    m: &Map<String, Value>,
    what: &str,
    key: &str,
    slot: &mut Vec<u32>,
) -> Result<(), String> {
    if let Some(v) = m.get(key) {
        *slot = list_of(what, key, v)?
            .iter()
            .map(|x| uint_of(what, key, x, u64::from(u32::MAX)).map(|n| n as u32))
            .collect::<Result<_, _>>()?;
    }
    Ok(())
}

fn set_f64s(
    m: &Map<String, Value>,
    what: &str,
    key: &str,
    slot: &mut Vec<f64>,
) -> Result<(), String> {
    if let Some(v) = m.get(key) {
        *slot = list_of(what, key, v)?
            .iter()
            .map(|x| f64_of(what, key, x))
            .collect::<Result<_, _>>()?;
    }
    Ok(())
}

fn set_units(m: &Map<String, Value>, what: &str, slot: &mut SigmaUnits) -> Result<(), String> {
    if let Some(v) = m.get("sigma_units") {
        let name = v
            .as_str()
            .ok_or_else(|| field_err(what, "sigma_units", "'vox' or 'mm'", v))?;
        *slot = parse_units(name, what)?;
    }
    Ok(())
}

fn linear_from_json(
    m: &Map<String, Value>,
    l: &mut LinearRecipe,
    what: &str,
) -> Result<(), String> {
    check_json_keys(m, LINEAR_KEYS, what)?;
    set_f64(m, what, "gradient_step", &mut l.gradient_step)?;
    set_u32s(m, what, "iterations", &mut l.iterations)?;
    set_f64(
        m,
        what,
        "convergence_threshold",
        &mut l.convergence_threshold,
    )?;
    set_usize(m, what, "convergence_window", &mut l.convergence_window)?;
    set_u32s(m, what, "shrink_factors", &mut l.shrink_factors)?;
    set_f64s(m, what, "smoothing_sigmas", &mut l.smoothing_sigmas)?;
    set_units(m, what, &mut l.sigma_units)?;
    set_usize(m, what, "bins", &mut l.bins)?;
    let mut sampling = f64::from(l.sampling_percentage);
    set_f64(m, what, "sampling_percentage", &mut sampling)?;
    l.sampling_percentage = sampling as f32;
    set_u32(m, what, "seed", &mut l.seed)?;
    Ok(())
}

fn syn_from_json(m: &Map<String, Value>, r: &mut SynRecipe, what: &str) -> Result<(), String> {
    check_json_keys(m, SYN_KEYS, what)?;
    set_f64(m, what, "gradient_step", &mut r.gradient_step)?;
    set_f64(
        m,
        what,
        "update_field_variance",
        &mut r.update_field_variance,
    )?;
    set_f64(m, what, "total_field_variance", &mut r.total_field_variance)?;
    set_usize(m, what, "cc_radius", &mut r.cc_radius)?;
    set_f64s(m, what, "channel_weights", &mut r.channel_weights)?;
    set_u32s(m, what, "iterations", &mut r.iterations)?;
    set_f64(
        m,
        what,
        "convergence_threshold",
        &mut r.convergence_threshold,
    )?;
    set_usize(m, what, "convergence_window", &mut r.convergence_window)?;
    set_u32s(m, what, "shrink_factors", &mut r.shrink_factors)?;
    set_f64s(m, what, "smoothing_sigmas", &mut r.smoothing_sigmas)?;
    set_units(m, what, &mut r.sigma_units)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_cohort_round_trips_through_json() {
        for channels in [1, 2] {
            let r = Recipe::cohort(channels);
            let back = Recipe::from_json(&r.to_json(), channels).unwrap();
            assert_eq!(back.to_json(), r.to_json());
        }
    }

    #[test]
    fn an_empty_object_is_the_cohort() {
        let r = Recipe::from_json(&json!({}), 1).unwrap();
        assert_eq!(r.to_json(), Recipe::cohort(1).to_json());
    }

    #[test]
    fn a_field_replaces_only_itself() {
        let r = Recipe::from_json(&json!({"syn": {"iterations": [5, 3]}, "rigid": null}), 1);
        let r = r.unwrap_err();
        assert!(r.contains("recipe['syn']"), "{r}");
        let r = Recipe::from_json(
            &json!({"syn": {"iterations": [5, 3], "shrink_factors": [2, 1],
                            "smoothing_sigmas": [1, 0]}}),
            1,
        )
        .unwrap();
        let syn = r.plan.syn.as_ref().unwrap();
        assert_eq!(syn.iterations, vec![5, 3]);
        assert_eq!(syn.smoothing_sigmas, vec![1.0, 0.0]);
        assert_eq!(
            r.plan.linear[0].iterations,
            Recipe::cohort(1).plan.linear[0].iterations
        );
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_refused() {
        let e = Recipe::from_json(&json!({"rigd": {}}), 1).unwrap_err();
        assert!(e.contains("unknown key 'rigd'"), "{e}");
        let e = Recipe::from_json(&json!({"rigid": {"iteration": [1]}}), 1).unwrap_err();
        assert!(
            e.contains("recipe['rigid']: unknown key 'iteration'"),
            "{e}"
        );
        let e = Recipe::from_json(&json!({"rigid": {"iterations": "100x50"}}), 1).unwrap_err();
        assert!(e.contains("recipe['rigid']['iterations']"), "{e}");
        let e = Recipe::from_json(&json!({"rigid": {"seed": -1}}), 1).unwrap_err();
        assert!(e.contains("recipe['rigid']['seed']"), "{e}");
        let e = Recipe::from_json(&json!({"syn": []}), 1).unwrap_err();
        assert!(e.contains("must be a dict"), "{e}");
        let e = Recipe::from_json(&json!([1]), 1).unwrap_err();
        assert!(e.contains("JSON object"), "{e}");
        let e = Recipe::from_json(&json!({"syn": {"iterations": "100x70"}}), 1).unwrap_err();
        assert!(e.contains("got a string"), "{e}");
        let e = Recipe::from_json(&json!({"syn": {"sigma_units": "cm"}}), 1).unwrap_err();
        assert!(e.contains("'vox' or 'mm'"), "{e}");
    }

    #[test]
    fn gpu_limits_are_checked() {
        let e = Recipe::from_json(&json!({"affine": {"bins": 64}}), 1).unwrap_err();
        assert!(e.contains("32 bins"), "{e}");
        let e = Recipe::from_json(&json!({"syn": {"cc_radius": 9}}), 1).unwrap_err();
        assert!(e.contains("cc_radius"), "{e}");
        let e = Recipe::from_json(&json!({}), 2);
        assert!(e.is_ok());
        let e = Recipe::from_json(&json!({"syn": {"channel_weights": [1.0]}}), 2).unwrap_err();
        assert!(e.contains("1 weights for 2 channel(s)"), "{e}");
    }

    #[test]
    fn a_linear_stage_reads_its_fields() {
        let s = linear_stage_from_json(Some(&json!({"iterations": [10, 5, 0, 0]})), "affine");
        assert_eq!(s.unwrap().iterations, vec![10, 5, 0, 0]);
        assert!(linear_stage_from_json(None, "similarity").is_err());
        assert!(linear_stage_from_json(Some(&json!(3)), "rigid").is_err());
    }

    #[test]
    fn the_cohort_form_maps_to_the_cohort_plan() {
        let r = Recipe::from_json(&json!({}), 2).unwrap();
        let cohort = StagePlan::cohort(&LinearPlan::cohort(), &SynRecipe::cohort_multispectral());
        assert_eq!(r.plan, cohort);
        assert!(r.to_json().get("winsorize").is_none());
        let r = Recipe::from_json(&json!({"winsorize": null}), 1).unwrap();
        assert_eq!(r.plan.winsorize, None);
        assert_eq!(r.to_json()["winsorize"], Value::Null);
        let back = Recipe::from_json(&r.to_json(), 1).unwrap();
        assert_eq!(back.plan, r.plan);
        let r = Recipe::from_json(&json!({"winsorize": [0.01, 0.99]}), 1).unwrap();
        assert_eq!(r.plan.winsorize, Some((0.01, 0.99)));
        assert_eq!(r.to_json()["winsorize"], json!([0.01, 0.99]));
        let r = Recipe::from_json(&json!({"winsorize": [0.005, 0.995]}), 1).unwrap();
        assert!(r.to_json().get("winsorize").is_none());
        for bad in [
            json!([0.5, 0.5]),
            json!([0.1]),
            json!("0.1,0.9"),
            json!([-0.1, 0.9]),
        ] {
            let e = Recipe::from_json(&json!({"winsorize": bad}), 1).unwrap_err();
            assert!(e.contains("recipe['winsorize']"), "{e}");
        }
    }

    #[test]
    fn the_stage_list_form_reads_any_linear_chain_and_an_optional_syn_stage() {
        let r = Recipe::from_json(
            &json!({"initial": "identity", "stages": [
                {"transform": "affine", "iterations": [5, 3], "shrink_factors": [2, 1],
                 "smoothing_sigmas": [1, 0]},
                {"transform": "rigid"},
                {"transform": "affine"},
            ]}),
            1,
        )
        .unwrap();
        assert_eq!(r.plan.initial, InitialTransform::Identity);
        assert_eq!(
            r.plan.linear.iter().map(|l| l.kind).collect::<Vec<_>>(),
            [LinearKind::Affine, LinearKind::Rigid, LinearKind::Affine]
        );
        assert_eq!(r.plan.linear[0].iterations, vec![5, 3]);
        assert!(r.plan.syn.is_none());
        let v = r.to_json();
        assert_eq!(v["initial"], json!("identity"));
        assert_eq!(v["stages"][1]["transform"], json!("rigid"));
        assert_eq!(v["winsorize"], json!([0.005, 0.995]));
        let back = Recipe::from_json(&v, 1).unwrap();
        assert_eq!(back.plan, r.plan);
        assert_eq!(back.to_json(), v);

        let r = Recipe::from_json(
            &json!({"stages": [{"transform": "syn", "cc_radius": 2}]}),
            2,
        )
        .unwrap();
        assert!(r.plan.linear.is_empty());
        let syn = r.plan.syn.as_ref().unwrap();
        assert_eq!(
            (syn.cc_radius, syn.channel_weights.clone()),
            (2, vec![0.5, 0.5])
        );
        assert_eq!(r.plan.initial, InitialTransform::CentreOfMass);

        let r = Recipe::from_json(&json!({"initial": "com"}), 1).unwrap();
        assert_eq!(r.plan.linear.len(), 2);
        assert!(r.plan.syn.is_some());
        assert!(r.to_json()["stages"].as_array().unwrap().len() == 3);

        let r = Recipe::from_json(
            &json!({"initial": {"parameters": [1, 0, 0, 0, 1, 0, 0, 0, 1, 2, 3, 4],
                                "center": [1, 1, 1]},
                    "stages": [{"transform": "rigid"}]}),
            1,
        )
        .unwrap();
        match r.plan.initial {
            InitialTransform::Affine(a) => {
                assert_eq!(a.translation, [2.0, 3.0, 4.0]);
                assert_eq!(a.center, [1.0, 1.0, 1.0]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(r.to_json()["initial"]["center"], json!([1.0, 1.0, 1.0]));
    }

    #[test]
    fn a_stage_list_is_refused_when_its_order_or_contents_are_not_supported() {
        let cases = [
            (json!({"stages": []}), "is empty"),
            (json!({"stages": {"transform": "rigid"}}), "must be a list"),
            (json!({"stages": ["rigid"]}), "must be a dict"),
            (
                json!({"stages": [{"transform": "similarity"}]}),
                "'rigid', 'affine' or 'syn'",
            ),
            (
                json!({"stages": [{"iterations": [1]}]}),
                "'rigid', 'affine' or 'syn'",
            ),
            (
                json!({"stages": [{"transform": "syn"}, {"transform": "affine"}]}),
                "after the SyN stage",
            ),
            (
                json!({"stages": [{"transform": "syn"}, {"transform": "syn"}]}),
                "after the SyN stage",
            ),
            (
                json!({"stages": [{"transform": "rigid", "cc_radius": 2}]}),
                "unknown key 'cc_radius'",
            ),
            (
                json!({"stages": [{"transform": "affine", "bins": 64}]}),
                "recipe['stages'][0]['bins']",
            ),
            (
                json!({"stages": [{"transform": "rigid"}, {"transform": "syn", "cc_radius": 9}]}),
                "recipe['stages'][1]['cc_radius']",
            ),
            (json!({"rigid": {}, "stages": []}), "cannot be combined"),
            (json!({"initial": "middle"}), "recipe['initial']"),
            (
                json!({"initial": {"path": "no/such/file.mat"}}),
                "no/such/file.mat",
            ),
            (
                json!({"initial": {"path": "a.mat", "invert": 1}}),
                "'invert'",
            ),
            (
                json!({"initial": {"parameters": [1, 2], "center": [0, 0, 0]}}),
                "12 numbers",
            ),
        ];
        for (v, needle) in cases {
            let e = Recipe::from_json(&v, 1).unwrap_err();
            assert!(e.contains(needle), "{v}: {e}");
        }
    }

    #[test]
    fn a_relative_start_path_is_anchored_and_a_linear_chain_takes_one_channel() {
        let v = json!({"initial": {"path": "a.mat"}, "stages": []});
        let r = resolve_initial_path(&v, std::path::Path::new("dir"));
        assert_eq!(
            r["initial"]["path"],
            json!(std::path::Path::new("dir").join("a.mat").to_string_lossy())
        );
        let abs = std::env::temp_dir().join("a.mat");
        let v = json!({"initial": {"path": abs.to_string_lossy()}});
        assert_eq!(resolve_initial_path(&v, std::path::Path::new("dir")), v);
        assert_eq!(
            resolve_initial_path(&json!({}), std::path::Path::new("dir")),
            json!({})
        );
        let e = Recipe::from_json(&json!({"stages": [{"transform": "rigid"}]}), 2).unwrap_err();
        assert!(e.contains("no SyN stage"), "{e}");
    }

    #[test]
    fn null_keys_keep_the_default_and_a_singular_start_is_named() {
        let r = Recipe::from_json(
            &json!({"initial": null, "stages": [{"transform": "rigid"}]}),
            1,
        )
        .unwrap();
        assert_eq!(r.plan.initial, InitialTransform::CentreOfMass);
        assert_eq!(r.plan.linear.len(), 1);
        let r = Recipe::from_json(&json!({"initial": "identity", "stages": null}), 1).unwrap();
        assert_eq!(r.plan.linear.len(), 2);
        assert!(r.plan.syn.is_some());
        let r = Recipe::from_json(&json!({"rigid": {"seed": 3}, "initial": null}), 1).unwrap();
        assert_eq!(r.plan.linear[0].seed, 3);
        let e = Recipe::from_json(
            &json!({"initial": {"parameters": [0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0],
                                "center": [0, 0, 0]}}),
            1,
        )
        .unwrap_err();
        assert_eq!(e, "the initial transform in recipe['initial'] is singular");
        let e = Recipe::from_json(&json!({"winsorize": [0.99999999, 1.0]}), 1).unwrap_err();
        assert!(e.starts_with("recipe['winsorize']"), "{e}");
    }

    #[test]
    fn a_start_file_is_read_and_may_be_inverted() {
        let d = std::env::temp_dir().join(format!("fastwarp_cli_recipe_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = AffineTransform::new(
            [[1.1, 0.0, 0.0], [0.0, 0.9, 0.1], [0.0, 0.0, 1.0]],
            [2.0, -1.0, 0.5],
            [3.0, 4.0, 5.0],
        );
        let mat = d.join("start.mat");
        fastwarp_reg::transform::itk_mat::write_mat_file(&mat, &[TransformElem::Affine(a)])
            .unwrap();
        let path = mat.to_string_lossy().into_owned();
        let r = Recipe::from_json(&json!({"initial": {"path": path}}), 1).unwrap();
        assert_eq!(r.plan.initial, InitialTransform::Affine(a));
        assert_eq!(
            r.to_json()["initial"],
            json!({"path": path, "invert": false})
        );
        let r = Recipe::from_json(&json!({"initial": {"path": path, "invert": true}}), 1).unwrap();
        match r.plan.initial {
            InitialTransform::Affine(inv) => {
                let p = [7.0, -2.0, 9.0];
                let q = inv.apply(a.apply(p));
                for k in 0..3 {
                    assert!((q[k] - p[k]).abs() < 1e-12);
                }
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&d).ok();
    }
}
